import os
import json
import shutil
import subprocess
import zipfile
import minecraft_launcher_lib


def microsoft_device_login(client_id):
    """Authenticate a Microsoft account using device code and Minecraft/Xbox services."""
    import time
    import requests

    device = requests.post(
        "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode",
        data={"client_id": client_id, "scope": "XboxLive.signin offline_access"},
        timeout=30,
    )
    device.raise_for_status()
    device_data = device.json()
    if "device_code" not in device_data:
        raise RuntimeError(f"Microsoft device-code request failed: {device_data}")

    print("GLIDE_STAGE=microsoft-login")
    print(f"MICROSOFT_LOGIN_URL={device_data.get('verification_uri')}")
    print(f"MICROSOFT_LOGIN_CODE={device_data.get('user_code')}")
    print("MICROSOFT_LOGIN_INSTRUCTIONS=Open the URL above, enter the code, and finish Microsoft sign-in.")
    print(f"MICROSOFT_LOGIN_EXPIRES_IN={device_data.get('expires_in')}")

    interval = max(int(device_data.get("interval", 5)), 5)
    deadline = time.time() + int(device_data.get("expires_in", 900))
    token_data = None
    while time.time() < deadline:
        response = requests.post(
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
            data={
                "client_id": client_id,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": device_data["device_code"],
            },
            timeout=30,
        )
        token_data = response.json()
        if "access_token" in token_data:
            break
        if token_data.get("error") not in ("authorization_pending", "slow_down"):
            raise RuntimeError(f"Microsoft login failed: {token_data}")
        time.sleep(interval + (5 if token_data.get("error") == "slow_down" else 0))

    if not token_data or "access_token" not in token_data:
        raise RuntimeError("Microsoft sign-in timed out before authorization completed.")

    microsoft_access_token = token_data["access_token"]
    xbl = minecraft_launcher_lib.microsoft_account.authenticate_with_xbl(microsoft_access_token)
    if "Token" not in xbl or not xbl.get("DisplayClaims", {}).get("xui"):
        raise RuntimeError(f"Xbox Live authentication failed: {xbl}")
    xsts = minecraft_launcher_lib.microsoft_account.authenticate_with_xsts(xbl["Token"])
    if "Token" not in xsts:
        raise RuntimeError(f"Xbox security-token authentication failed: {xsts}")
    account = minecraft_launcher_lib.microsoft_account.authenticate_with_minecraft(
        xbl["DisplayClaims"]["xui"][0]["uhs"], xsts["Token"]
    )
    if "access_token" not in account:
        raise RuntimeError(f"Minecraft authentication failed: {account}")
    profile = minecraft_launcher_lib.microsoft_account.get_profile(account["access_token"])
    if profile.get("error") == "NOT_FOUND":
        raise RuntimeError("This Microsoft account does not own Minecraft: Java Edition.")
    if "id" not in profile or "name" not in profile:
        raise RuntimeError(f"Minecraft profile lookup failed: {profile}")
    profile["access_token"] = account["access_token"]
    profile["refresh_token"] = token_data.get("refresh_token", "")
    print(f"MINECRAFT_ACCOUNT_NAME={profile['name']}")
    print(f"MINECRAFT_ACCOUNT_UUID={profile['id']}")
    print("MICROSOFT_LOGIN_READY=true")
    return profile


def validate_skin_png(path):
    """Validate a Minecraft skin PNG without adding a Pillow dependency."""
    import struct
    with open(path, "rb") as skin_file:
        header = skin_file.read(24)
    if len(header) < 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise RuntimeError(f"Skin is not a valid PNG: {path}")
    width, height = struct.unpack(">II", header[16:24])
    if (width, height) not in {(64, 64), (64, 32)}:
        raise RuntimeError(f"Skin must be 64x64 or legacy 64x32 PNG: {path} (got {width}x{height})")


def offline_player_uuid(username):
    """Return the UUID used by Java for an offline Minecraft profile."""
    import hashlib
    import uuid

    profile_name = f"OfflinePlayer:{username}".encode("utf-8")
    return uuid.UUID(bytes=hashlib.md5(profile_name).digest(), version=3)


def normalize_loader_name(value):
    aliases = {
        "default": "vanilla",
        "vanilla": "vanilla",
        "fabric": "fabric",
        "fabric-loader": "fabric",
        "legacy-fabric": "legacy-fabric",
        "legacyfabric": "legacy-fabric",
        "forge": "forge",
        "legacy-forge": "legacy-forge",
        "legacyforge": "legacy-forge",
        "quilt": "quilt",
        "neo-forge": "neoforge",
        "neoforge": "neoforge",
    }
    if value is None:
        return "vanilla"
    normalized = value.strip().lower().replace("_", "-").replace(" ", "-")
    normalized = normalized.replace("loader", "").strip("-")
    if normalized == "":
        return "vanilla"
    return aliases.get(normalized, normalized)


def resolve_window_size(preset=None, width=None, height=None):
    presets = {
        "tiny": (640, 360),
        "low": (854, 480),
        "balanced": (1280, 720),
        "high": (1600, 900),
        "ultra": (1920, 1080),
    }
    key = (preset or os.environ.get("GLIDE_RESOLUTION_PRESET", "balanced")).strip().lower()
    if key == "custom":
        try:
            target_width = int(width if width is not None else os.environ.get("GLIDE_RENDER_WIDTH", "1280"))
            target_height = int(height if height is not None else os.environ.get("GLIDE_RENDER_HEIGHT", "720"))
        except ValueError:
            target_width, target_height = presets["balanced"]
        return max(target_width, 1), max(target_height, 1)
    chosen = presets.get(key, presets["balanced"])
    if width is not None or height is not None:
        target_width = int(width if width is not None else chosen[0])
        target_height = int(height if height is not None else chosen[1])
        return max(target_width, 1), max(target_height, 1)
    return chosen


def upload_skin(profile, skin_path, model):
    """Upload the repository skin to the authenticated Minecraft profile."""
    import requests
    validate_skin_png(skin_path)
    with open(skin_path, "rb") as skin_file:
        response = requests.post(
            "https://api.minecraftservices.com/minecraft/profile/skins",
            headers={"Authorization": f"Bearer {profile['access_token']}"},
            files={"file": (os.path.basename(skin_path), skin_file, "image/png")},
            data={"variant": model},
            timeout=60,
        )
    if response.status_code >= 400:
        raise RuntimeError(f"Skin upload failed ({response.status_code}): {response.text[:500]}")
    print(f"MINECRAFT_SKIN_APPLIED={os.path.basename(skin_path)}")


version = os.environ["GLIDE_MINECRAFT_VERSION"]
minecraft_dir = "/home/runner/glide-minecraft"
xmx = os.environ["GLIDE_MINECRAFT_XMX"]
loader = normalize_loader_name(os.environ.get("GLIDE_MINECRAFT_LOADER", "vanilla"))
window_width, window_height = resolve_window_size(
    os.environ.get("GLIDE_RESOLUTION_PRESET", "balanced"),
    os.environ.get("GLIDE_RENDER_WIDTH"),
    os.environ.get("GLIDE_RENDER_HEIGHT"),
)

print("GLIDE_STAGE=minecraft-download")
print(f"Preparing official Minecraft {version} client files...")
minecraft_launcher_lib.install.install_minecraft_version(version, minecraft_dir)
print("MINECRAFT_FILES_READY=true")

if version == "1.12.2":
    import hashlib
    import json
    import urllib.request
    vanilla_jar = os.path.join(minecraft_dir, "versions", version, f"{version}.jar")
    if not os.path.isfile(vanilla_jar) or os.path.getsize(vanilla_jar) < 1000000:
        raise RuntimeError(f"Vanilla client jar is missing or unexpectedly small: {vanilla_jar}")
    with open(vanilla_jar, "rb") as jar_file:
        vanilla_size = os.path.getsize(vanilla_jar)
        vanilla_sha1 = hashlib.sha1(jar_file.read()).hexdigest()
    print(f"MINECRAFT_VANILLA_JAR={vanilla_jar}")
    print(f"MINECRAFT_VANILLA_JAR_SIZE={vanilla_size}")
    print(f"MINECRAFT_VANILLA_JAR_SHA1={vanilla_sha1}")
    if vanilla_size < 5000000:
        raise RuntimeError("Minecraft 1.12.2 client jar is unexpectedly small; refusing to launch Forge")

mods_dir = os.path.join(minecraft_dir, "mods")
os.makedirs(mods_dir, exist_ok=True)

shaderpacks_dir = os.path.join(minecraft_dir, "shaderpacks")
os.makedirs(shaderpacks_dir, exist_ok=True)
resourcepacks_dir = os.path.join(minecraft_dir, "resourcepacks")
os.makedirs(resourcepacks_dir, exist_ok=True)
repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
direct_shader_dir = os.path.join(repo_root, "Shaders")
resourcepack_source_dir = os.path.join(repo_root, "resourcepacks")
shader_sources = []
if os.path.isdir(resourcepack_source_dir):
    for entry in sorted(os.listdir(resourcepack_source_dir)):
        source = os.path.join(resourcepack_source_dir, entry)
        if os.path.isdir(source):
            target = os.path.join(resourcepacks_dir, entry)
            if os.path.exists(target):
                shutil.rmtree(target)
            shutil.copytree(source, target)
        elif entry.lower().endswith((".zip", ".mcpack", ".jar")):
            shutil.copy2(source, os.path.join(resourcepacks_dir, entry))

if os.path.isdir(direct_shader_dir):
    shader_sources.extend(
        os.path.join(direct_shader_dir, name)
        for name in os.listdir(direct_shader_dir)
        if name.lower().endswith(".zip")
    )

import shutil
import zipfile
staged_shader_names = set()

for source in shader_sources:
    name = os.path.basename(source)
    shutil.copy2(source, os.path.join(shaderpacks_dir, name))
    staged_shader_names.add(name)

mods_source_root = os.path.join(repo_root, "mods")
if os.path.isdir(mods_source_root):
    for root, _, names in os.walk(mods_source_root):
        for archive_name in names:
            if not archive_name.lower().endswith((".zip", ".jar")):
                continue
            archive_path = os.path.join(root, archive_name)
            try:
                with zipfile.ZipFile(archive_path) as archive:
                    for member in archive.namelist():
                        normalized = member.replace("\\", "/")
                        if "/shaderpacks/" not in f"/{normalized}" or not normalized.lower().endswith(".zip"):
                            continue
                        name = os.path.basename(normalized)
                        if not name or name in staged_shader_names:
                            continue
                        with archive.open(member) as source_file, open(
                            os.path.join(shaderpacks_dir, name), "wb"
                        ) as destination_file:
                            shutil.copyfileobj(source_file, destination_file)
                        staged_shader_names.add(name)
            except (zipfile.BadZipFile, OSError):
                continue

print(f"MINECRAFT_SHADERPACK_COUNT={len(staged_shader_names)}")
for name in sorted(staged_shader_names):
    print(f"MINECRAFT_SHADERPACK={name}")
mod_files = sorted(
    name for name in os.listdir(mods_dir)
    if name.lower().endswith(".jar")
)
print(f"MINECRAFT_MOD_COUNT={len(mod_files)}")
for name in mod_files:
    print(f"MINECRAFT_MOD={name}")

if version == "1.12.2" and "_MixinBootstrap-1.1.0.jar" in mod_files:
    import urllib.request
    mixinbooter_legacy = os.path.join(mods_dir, "!mixinbooter-10.7.jar")
    if not os.path.isfile(mixinbooter_legacy):
        mixinbooter_url = (
            "https://maven.cleanroommc.com/zone/rong/mixinbooter/10.7/"
            "mixinbooter-10.7.jar"
        )
        print("GLIDE_STAGE=mixinbooter-download")
        print(f"Downloading MixinBooter 10.7 compatibility library: {mixinbooter_url}")
        urllib.request.urlretrieve(mixinbooter_url, mixinbooter_legacy)
    print(f"MINECRAFT_MIXINBOOTER={mixinbooter_legacy}")

launch_version = version
if loader in {"fabric", "legacy-fabric"}:
    legacy_fabric = loader == "legacy-fabric"
    print("GLIDE_STAGE=fabric-install")
    if version in {"1.12.2", "1.13", "1.13.1", "1.13.2"} and not legacy_fabric:
        raise RuntimeError(f"Fabric requires Minecraft 1.14 or newer; requested {version}")

    if legacy_fabric:
        print("MINECRAFT_FABRIC_FLAVOR=legacy")
    else:
        print("MINECRAFT_FABRIC_FLAVOR=modern")

    print("Installing Fabric Loader through minecraft-launcher-lib...")
    minecraft_launcher_lib.fabric.install_fabric(version, minecraft_dir)
    installed = minecraft_launcher_lib.utils.get_installed_versions(minecraft_dir)
    candidates = [
        item["id"] for item in installed
        if item.get("id", "").startswith("fabric-loader-")
    ]
    if not candidates:
        raise RuntimeError(f"Fabric installation produced no fabric-loader profile for {version}")

    matching = []
    for item in installed:
        profile_id = item.get("id", "")
        if not profile_id.startswith("fabric-loader-"):
            continue
        profile_path = os.path.join(
            minecraft_dir, "versions", profile_id, f"{profile_id}.json"
        )
        try:
            with open(profile_path, "r", encoding="utf-8") as profile_file:
                profile = json.load(profile_file)
        except (OSError, ValueError):
            continue
        inherits = profile.get("inheritsFrom", "")
        if inherits == version:
            matching.append(profile_id)

    if not matching:
        raise RuntimeError(
            f"Fabric installation produced profiles, but none inherit Minecraft {version}: "
            f"{candidates}"
        )
    launch_version = matching[-1]
    profile_path = os.path.join(minecraft_dir, "versions", launch_version, f"{launch_version}.json")
    if not os.path.isfile(profile_path):
        raise RuntimeError(f"Fabric profile JSON was not created: {profile_path}")
    print(f"MINECRAFT_LOADER_VERSION={launch_version}")
    print(f"MINECRAFT_FABRIC_PROFILE={profile_path}")
elif loader in {"forge", "legacy-forge"}:
    legacy_forge = loader == "legacy-forge"
    print("GLIDE_STAGE=forge-install")
    if legacy_forge:
        print("MINECRAFT_FORGE_FLAVOR=legacy")
    else:
        print("MINECRAFT_FORGE_FLAVOR=modern")
    forge_version = minecraft_launcher_lib.forge.find_forge_version(version)
    if not forge_version:
        raise RuntimeError(f"No Forge version found for Minecraft {version}")
    print(f"MINECRAFT_FORGE_VERSION={forge_version}")
    if minecraft_launcher_lib.forge.supports_automatic_install(forge_version):
        minecraft_launcher_lib.forge.install_forge_version(forge_version, minecraft_dir)
        launch_version = minecraft_launcher_lib.forge.forge_to_installed_version(forge_version)
    else:
        import subprocess as sp
        installer = os.path.join("/tmp", f"forge-{forge_version}-installer.jar")
        installer_url = f"https://maven.minecraftforge.net/net/minecraftforge/forge/{forge_version}/forge-{forge_version}-installer.jar"
        print(f"Downloading Forge installer: {installer_url}")
        print("GLIDE_STAGE=forge-installer-download")
        sp.run([
            "curl", "--fail", "--location", "--retry", "4",
            "--retry-all-errors", "--connect-timeout", "15",
            "--max-time", "180", "--user-agent",
            "Glide/1.0 (+https://github.com/PierrotTheFreak/Glide)",
            "--output", installer, installer_url,
        ], check=True)
        if not os.path.isfile(installer) or os.path.getsize(installer) < 100000:
            raise RuntimeError("Forge installer download was missing or unexpectedly small")
        print(f"FORGE_INSTALLER_SIZE={os.path.getsize(installer)}")

        import json
        launcher_profiles = os.path.join(minecraft_dir, "launcher_profiles.json")
        if not os.path.exists(launcher_profiles):
            with open(launcher_profiles, "w", encoding="utf-8") as profile_file:
                json.dump({
                    "profiles": {
                        "Glide": {
                            "name": "Glide",
                            "type": "custom",
                            "lastVersionId": version,
                        }
                    },
                    "selectedProfile": "Glide",
                }, profile_file)
            print("MINECRAFT_LAUNCHER_PROFILE_CREATED=true")
        else:
            print("MINECRAFT_LAUNCHER_PROFILE_CREATED=false")

        print("GLIDE_STAGE=forge-official-installer")
        sp.run([
            "java", "-jar", installer, "--installClient", minecraft_dir
        ], check=True)
        launch_version = minecraft_launcher_lib.forge.forge_to_installed_version(forge_version)

        if version == "1.12.2":
            import shutil
            forge_version_dir = os.path.join(minecraft_dir, "versions", launch_version)
            forge_version_jar = os.path.join(forge_version_dir, f"{launch_version}.jar")
            vanilla_jar = os.path.join(minecraft_dir, "versions", version, f"{version}.jar")
            if not os.path.isfile(vanilla_jar):
                raise RuntimeError(f"Verified vanilla client jar disappeared: {vanilla_jar}")
            os.makedirs(forge_version_dir, exist_ok=True)
            if not os.path.isfile(forge_version_jar):
                shutil.copy2(vanilla_jar, forge_version_jar)
                print(f"MINECRAFT_LEGACY_FORGE_JAR_COPIED={forge_version_jar}")
            print(f"MINECRAFT_LEGACY_FORGE_JAR_SIZE={os.path.getsize(forge_version_jar)}")
    print(f"MINECRAFT_LOADER_VERSION={launch_version}")
elif loader == "quilt":
    print("GLIDE_STAGE=quilt-install")
    quilt_mod = getattr(minecraft_launcher_lib, "quilt", None)
    if quilt_mod is None or not hasattr(quilt_mod, "install_quilt"):
        raise RuntimeError("Quilt support is not available in this minecraft-launcher-lib build.")
    quilt_mod.install_quilt(version, minecraft_dir)
    installed = minecraft_launcher_lib.utils.get_installed_versions(minecraft_dir)
    candidates = [item["id"] for item in installed if item.get("id", "").startswith("quilt-loader-")]
    if not candidates:
        raise RuntimeError(f"Quilt installation produced no quilt-loader profile for {version}")
    launch_version = candidates[-1]
    print(f"MINECRAFT_LOADER_VERSION={launch_version}")
elif loader == "neoforge":
    print("GLIDE_STAGE=neoforge-install")
    neoforge_mod = getattr(minecraft_launcher_lib, "neoforge", None)
    if neoforge_mod is None or not hasattr(neoforge_mod, "install_neoforge_version"):
        raise RuntimeError("NeoForge support is not available in this minecraft-launcher-lib build.")
    version_key = getattr(neoforge_mod, "find_neoforge_version", lambda _: None)(version)
    if not version_key:
        raise RuntimeError(f"No NeoForge version found for Minecraft {version}")
    neoforge_mod.install_neoforge_version(version_key, minecraft_dir)
    launch_version = getattr(neoforge_mod, "neoforge_to_installed_version", lambda _: version_key)(version_key)
    print(f"MINECRAFT_LOADER_VERSION={launch_version}")

print("GLIDE_STAGE=minecraft-options")
options = minecraft_launcher_lib.utils.generate_test_options()

auth_mode = os.environ.get("GLIDE_AUTH_MODE", "offline").strip().lower()
if auth_mode == "microsoft":
    client_id = os.environ.get("GLIDE_MICROSOFT_CLIENT_ID", "").strip()
    if not client_id:
        raise RuntimeError("GLIDE_MICROSOFT_CLIENT_ID is required for Microsoft authentication.")
    login_data = microsoft_device_login(client_id)
    options.update({
        "username": login_data["name"],
        "uuid": login_data["id"],
        "token": login_data["access_token"],
    })

    skin_dir = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "skins")
    skin_files = sorted(
        os.path.join(skin_dir, name)
        for name in os.listdir(skin_dir)
        if name.lower().endswith(".png")
    ) if os.path.isdir(skin_dir) else []
    if len(skin_files) > 1:
        raise RuntimeError("skins/ may contain at most one PNG skin.")
    if skin_files:
        upload_skin(login_data, skin_files[0], os.environ.get("GLIDE_SKIN_MODEL", "classic"))
elif auth_mode == "offline":
    import re
    offline_username = os.environ.get("GLIDE_OFFLINE_USERNAME", "").strip()
    if not re.fullmatch(r"[A-Za-z0-9_]{3,16}", offline_username):
        raise RuntimeError("GLIDE_OFFLINE_USERNAME must be 3-16 characters using only letters, numbers, or underscores.")
    offline_uuid = offline_player_uuid(offline_username)
    options.update({"username": offline_username, "uuid": str(offline_uuid), "token": ""})
    print(f"MINECRAFT_OFFLINE_USERNAME={offline_username}")
    print(f"MINECRAFT_OFFLINE_UUID={offline_uuid}")
    print("MINECRAFT_OFFLINE_PROFILE_READY=true")
else:
    raise RuntimeError("GLIDE_AUTH_MODE must be either offline or microsoft.")


native_source_version = version if loader in {"fabric", "legacy-fabric", "quilt"} else launch_version
natives_dir = os.path.join(
    minecraft_dir, "versions", native_source_version, "natives"
)
print("GLIDE_STAGE=minecraft-natives")
print(f"MINECRAFT_NATIVE_SOURCE_VERSION={native_source_version}")
minecraft_launcher_lib.natives.extract_natives(
    native_source_version, minecraft_dir, natives_dir
)

if not os.path.isdir(natives_dir) or not os.listdir(natives_dir):
    import urllib.request
    import zipfile

    def load_manifest(profile_id, seen=None):
        if seen is None:
            seen = set()
        if profile_id in seen:
            return {}
        seen.add(profile_id)
        manifest_path = os.path.join(
            minecraft_dir, "versions", profile_id, f"{profile_id}.json"
        )
        with open(manifest_path, "r", encoding="utf-8") as manifest_file:
            manifest = json.load(manifest_file)
        merged = {}
        parent = manifest.get("inheritsFrom")
        if parent:
            merged = load_manifest(parent, seen)
        libraries = list(merged.get("libraries", []))
        libraries.extend(manifest.get("libraries", []))
        merged.update(manifest)
        merged["libraries"] = libraries
        return merged

    manifest = load_manifest(native_source_version)
    os.makedirs(natives_dir, exist_ok=True)
    extracted_native_jars = 0
    for library in manifest.get("libraries", []):
        name = library.get("name", "")
        if "natives-linux" not in name:
            continue
        downloads = library.get("downloads", {})
        artifact = downloads.get("artifact")
        if not artifact or not artifact.get("url"):
            continue

        native_url = artifact["url"]
        native_sha1 = artifact.get("sha1")
        native_name = os.path.basename(
            artifact.get("path", native_url)
        )
        native_path = os.path.join(
            minecraft_dir,
            "libraries",
            artifact.get("path", native_name),
        )
        os.makedirs(os.path.dirname(native_path), exist_ok=True)

        if not os.path.isfile(native_path):
            print(f"Downloading Linux native library: {name}")
            urllib.request.urlretrieve(native_url, native_path)

        if native_sha1:
            import hashlib
            with open(native_path, "rb") as native_file:
                actual_sha1 = hashlib.sha1(native_file.read()).hexdigest()
            if actual_sha1 != native_sha1:
                raise RuntimeError(
                    f"Native library checksum mismatch for {name}: "
                    f"{actual_sha1} != {native_sha1}"
                )

        with zipfile.ZipFile(native_path) as native_zip:
            native_zip.extractall(natives_dir)
        extracted_native_jars += 1

    print(f"MINECRAFT_NATIVE_ARTIFACT_COUNT={extracted_native_jars}")

import shutil
native_so_files = []
if os.path.isdir(natives_dir):
    for root, _, files in os.walk(natives_dir):
        for native_name in files:
            if not native_name.endswith(".so"):
                continue
            source_path = os.path.join(root, native_name)
            target_path = os.path.join(natives_dir, native_name)
            if os.path.abspath(source_path) != os.path.abspath(target_path):
                if os.path.exists(target_path):
                    os.remove(target_path)
                shutil.copy2(source_path, target_path)
            native_so_files.append(native_name)
    native_so_files = sorted(set(native_so_files))

native_files = sorted(os.listdir(natives_dir)) if os.path.isdir(natives_dir) else []
print(f"MINECRAFT_NATIVES_DIR={natives_dir}")
print(f"MINECRAFT_NATIVE_FILE_COUNT={len(native_files)}")
for native_name in native_files:
    print(f"MINECRAFT_NATIVE_FILE={native_name}")
print(f"MINECRAFT_NATIVE_SO_COUNT={len(native_so_files)}")
for native_name in native_so_files:
    print(f"MINECRAFT_NATIVE_SO={native_name}")
if not any(
    name in {"liblwjgl64.so", "liblwjgl.so"} for name in native_so_files
):
    raise RuntimeError(
        "Minecraft native extraction completed but no LWJGL Linux native was found "
        f"(extracted .so files: {native_so_files})"
    )
options["nativesDirectory"] = natives_dir
print("MINECRAFT_NATIVES_READY=true")
print("MINECRAFT_TEST_OPTIONS_READY=true")

runtime_agent = os.environ.get("GLIDE_RUNTIME_AGENT", "").strip()

print("GLIDE_STAGE=minecraft-command")
command = minecraft_launcher_lib.command.get_minecraft_command(
    launch_version,
    minecraft_dir,
    options,
)

if runtime_agent:
    if not os.path.isfile(runtime_agent):
        raise RuntimeError(f"Configured Glide runtime agent does not exist: {runtime_agent}")
    command.insert(1, f"-javaagent:{runtime_agent}")
    print(f"GLIDE_RUNTIME_AGENT_ACTIVE={runtime_agent}")
else:
    print("GLIDE_RUNTIME_AGENT_ACTIVE=false")


native_arg = f"-Djava.library.path={natives_dir}"
lwjgl_native_arg = f"-Dorg.lwjgl.librarypath={natives_dir}"
if not any(arg.startswith("-Djava.library.path=") for arg in command):
    command.insert(1, native_arg)
if not any(arg.startswith("-Dorg.lwjgl.librarypath=") for arg in command):
    command.insert(2, lwjgl_native_arg)

command.insert(1, f"-Xmx{xmx}")
command.insert(2, "-Xms2G")
if version == "1.12.2":
    command.insert(3, "-XX:+UseG1GC")
    command.insert(4, "-XX:MaxGCPauseMillis=50")
    print("GLIDE_GC=G1")
else:
    command.insert(3, "-XX:+UseZGC")
    print("GLIDE_GC=ZGC")
command.insert(5 if version == "1.12.2" else 4, "-XX:+DisableExplicitGC")
command.extend(["--width", str(window_width), "--height", str(window_height)])

print("MINECRAFT_COMMAND_READY=true")
print("GLIDE_STAGE=minecraft-running")
print("GLIDE_READY=true")

restart_limit = max(int(os.environ.get("GLIDE_MAX_RESTARTS", "3")), 0)
restart_delay = max(float(os.environ.get("GLIDE_RESTART_DELAY", "3")), 0.0)
restart_count = 0

runtime_env = {
    **os.environ,
    "DISPLAY": ":99",
    "XDG_RUNTIME_DIR": os.environ.get("XDG_RUNTIME_DIR", "/tmp/glide-runtime"),
    "PULSE_RUNTIME_PATH": os.environ.get("PULSE_RUNTIME_PATH", "/tmp/glide-runtime/pulse"),
    "PULSE_SERVER": os.environ.get("PULSE_SERVER", "unix:/tmp/glide-runtime/pulse/native"),
    "PULSE_LATENCY_MSEC": os.environ.get("PULSE_LATENCY_MSEC", "60"),
    "PULSE_SUSPEND_ON_IDLE": os.environ.get("PULSE_SUSPEND_ON_IDLE", "no"),
    "PULSE_SINK": os.environ.get("PULSE_SINK", "glide_game_sink"),
    "PULSE_SOURCE": os.environ.get("PULSE_SOURCE", "glide_game_sink.monitor"),
    "SDL_AUDIODRIVER": os.environ.get("SDL_AUDIODRIVER", "pulseaudio"),
    "AUDIODRIVER": os.environ.get("AUDIODRIVER", "pulseaudio"),
    "ALSOFT_DRIVERS": os.environ.get("ALSOFT_DRIVERS", "pulse"),
    "GLIDE_RENDER_WIDTH": str(window_width),
    "GLIDE_RENDER_HEIGHT": str(window_height),
    "LD_PRELOAD": os.environ.get("LD_PRELOAD", ""),
}

while True:
    print(f"GLIDE_MINECRAFT_ATTEMPT={restart_count + 1}")
    with open("/tmp/glide-minecraft.log", "w", buffering=1) as log:
        process = subprocess.Popen(
            command,
            cwd=minecraft_dir,
            env=runtime_env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        print(f"MINECRAFT_PID={process.pid}")
        code = process.wait()
        print(f"MINECRAFT_EXIT_CODE={code}")

    if code == 0:
        print("MINECRAFT_EXIT_REASON=clean")
        break

    if restart_count >= restart_limit:
        print(f"MINECRAFT_RESTART_LIMIT_REACHED={restart_limit}")
        break

    restart_count += 1
    print(f"MINECRAFT_CRASH_RESTARTING=true")
    print(f"MINECRAFT_RESTART_DELAY={restart_delay}")
    import time
    time.sleep(restart_delay)
    continue

if code != 0:
    print("MINECRAFT_CRASH_LOG_BEGIN=true")
    try:
        with open("/tmp/glide-minecraft.log", "r", encoding="utf-8", errors="replace") as crash_log:
            lines = crash_log.readlines()
        for line in lines[-300:]:
            print(line, end="")
    except OSError as error:
        print(f"Could not read Minecraft log: {error}")

    crash_dir = os.path.join(minecraft_dir, "crash-reports")
    if os.path.isdir(crash_dir):
        reports = sorted(
            (
                os.path.join(crash_dir, name)
                for name in os.listdir(crash_dir)
                if name.endswith(".txt")
            ),
            key=lambda path: os.path.getmtime(path),
            reverse=True,
        )
        if reports:
            latest_report = reports[0]
            print(f"MINECRAFT_CRASH_REPORT={latest_report}")
            try:
                with open(latest_report, "r", encoding="utf-8", errors="replace") as report:
                    print(report.read())
            except OSError as error:
                print(f"Could not read crash report: {error}")

    for root in (minecraft_dir, "/tmp"):
        if not os.path.isdir(root):
            continue
        for name in sorted(os.listdir(root)):
            if name.startswith("hs_err_pid") and name.endswith(".log"):
                path = os.path.join(root, name)
                print(f"JVM_FATAL_REPORT={path}")
                try:
                    with open(path, "r", encoding="utf-8", errors="replace") as report:
                        print(report.read())
                except OSError as error:
                    print(f"Could not read JVM fatal report: {error}")
    print("MINECRAFT_CRASH_LOG_END=true")
if code != 0:
    raise SystemExit(code)
