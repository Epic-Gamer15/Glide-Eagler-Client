# Running Glide

1. Create a GitHub repo and push this folder to it (the game runs in GitHub Actions *on your repo*).
2. Make a GitHub token (repo + workflow/actions permissions): github.com/settings/tokens
3. Double-click `run.bat` (Windows) or run `./run.sh` (Mac/Linux). Needs Python 3.
   The panel opens at http://localhost:8080
4. Paste your token -> Check GitHub -> pick your repo -> Client -> Start.
   When the Actions run is ready (a few minutes), the stream appears in the page.

Supported versions: 1.20.1, 1.21.1, 1.21.4 (see Client/versions/versions.json).
Server mode additionally needs the PLAYIT_SECRET repo secret (see README.md).

## No local install: host on GitHub Pages
1. Push this folder to your repo's `main` branch.
2. Repo -> Settings -> Pages -> Source: **GitHub Actions**.
3. Run the "Deploy Glide panel to GitHub Pages" workflow (or push a change to `Html/`).
4. Open `https://<your-username>.github.io/<repo-name>/` from any browser.
