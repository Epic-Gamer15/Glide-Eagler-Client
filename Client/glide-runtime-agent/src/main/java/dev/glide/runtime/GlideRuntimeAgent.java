package dev.glide.runtime;

//Keep thisagent version agnostic Minecraft is weird and changes alot

import java.io.BufferedWriter;
import java.io.File;
import java.io.IOException;
import java.lang.management.ManagementFactory;
import java.lang.management.ThreadInfo;
import java.lang.management.ThreadMXBean;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.atomic.AtomicBoolean;


// Version-agnostic Minecraft runtime observer.
// This agent on purpose does not reference Minecraft/Fabric/Forge classes.
// It observes the JVM that is actually running and infers rendering pressure
// from live thread names and stack traces.

public final class GlideRuntimeAgent {
    private static final String OUTPUT =
            System.getenv().getOrDefault("GLIDE_RUNTIME_PROBE", "/tmp/glide-runtime.json");
    private static final AtomicBoolean RUNNING = new AtomicBoolean(true);

    private GlideRuntimeAgent() {}

    public static void premain(String args) { start(); }
    public static void agentmain(String args) { start(); }

    private static void start() {
        Thread probe = new Thread(GlideRuntimeAgent::run, "Glide-Runtime-Probe");
        probe.setDaemon(true);
        probe.setPriority(Thread.MIN_PRIORITY);
        Runtime.getRuntime().addShutdownHook(
                new Thread(() -> RUNNING.set(false), "Glide-Runtime-Probe-Stop"));
        probe.start();
        System.out.println("GLIDE_RUNTIME_PROBE_STARTED=true");
        System.out.println("GLIDE_RUNTIME_PROBE_FILE=" + OUTPUT);
    }

    private static void run() {
        ThreadMXBean bean = ManagementFactory.getThreadMXBean();
        boolean cpu = bean.isThreadCpuTimeSupported();
        if (cpu && !bean.isThreadCpuTimeEnabled()) {
            try { bean.setThreadCpuTimeEnabled(true); } catch (SecurityException ignored) {}
        }

        long previousWall = System.nanoTime();
        long previousTotal = totalCpu(bean);
        long previousRender = 0L;
        final int processors = Math.max(1, Runtime.getRuntime().availableProcessors());

        while (RUNNING.get()) {
            try {
                Thread.sleep(200L);
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
                break;
            }

            long now = System.nanoTime();
            long total = totalCpu(bean);
            long render = renderCpu(bean);
            long wall = Math.max(1L, now - previousWall);
            long totalDelta = Math.max(0L, total - previousTotal);
            long renderDelta = Math.max(0L, render - previousRender);

            double totalBusy = clamp((double) totalDelta / (double) wall / processors);
            double renderBusy = clamp((double) renderDelta / (double) wall / processors);

            tuneChunkThreadPriority(bean, totalBusy, renderBusy);
            writeSnapshot(bean, totalBusy, renderBusy);
            previousWall = now;
            previousTotal = total;
            previousRender = render;
        }
    }


// Hi code people
// if you ss this text and send it in the memes and media channel 
// Ill give you a custom rank of ur choosing (u choose name and color not perms)


    private static void tuneChunkThreadPriority(
            ThreadMXBean bean, double totalBusy, double renderBusy) {
        int priority;
        if (renderBusy >= 0.70 || totalBusy >= 0.92) {
            priority = Thread.MIN_PRIORITY;
        } else if (renderBusy >= 0.35 || totalBusy >= 0.75) {
            priority = Math.max(Thread.MIN_PRIORITY, Thread.NORM_PRIORITY - 2);
        } else {
            priority = Thread.NORM_PRIORITY + 1;
        }

        for (long id : bean.getAllThreadIds()) {
            ThreadInfo info = bean.getThreadInfo(id, 8);
            if (info == null || !looksChunkRelated(info)) continue;
            Thread thread = findLiveThread(id);
            if (thread == null || thread.getId() != id) continue;
            try {
                thread.setPriority(priority);
            } catch (SecurityException ignored) {
            }
        }
    }

    private static boolean looksChunkRelated(ThreadInfo info) {
        String name = info.getThreadName().toLowerCase(Locale.ROOT);
        if (containsAny(name, "render thread", "client thread", "server thread",
                "main", "glide-vulkan-render", "glide-runtime-probe")) {
            return false;
        }

        StringBuilder text = new StringBuilder(name);
        for (StackTraceElement frame : info.getStackTrace()) {
            text.append(' ').append(frame.getClassName().toLowerCase(Locale.ROOT));
            text.append(' ').append(frame.getMethodName().toLowerCase(Locale.ROOT));
        }
        String s = text.toString();

        return containsAny(name,
                "chunk", "terrain", "worldgen", "world-gen",
                "chunk build", "chunk render", "worker-main",
                "worker-", "server-worker")
                || containsAny(s,
                "chunkgenerator", "chunkgeneratorstatus",
                "chunkrenderdispatcher", "renderchunkrebuild",
                "terrainrenderdispatcher", "worldgen");
    }

    private static Thread findLiveThread(long id) {
        for (Thread thread : Thread.getAllStackTraces().keySet()) {
            if (thread.getId() == id) return thread;
        }
        return null;
    }

    private static long totalCpu(ThreadMXBean bean) {
        long total = 0L;
        long[] ids = bean.getAllThreadIds();
        for (long id : ids) {
            long value = bean.getThreadCpuTime(id);
            if (value > 0) total += value;
        }
        return total;
    }

    private static long renderCpu(ThreadMXBean bean) {
        long total = 0L;
        long[] ids = bean.getAllThreadIds();
        for (long id : ids) {
            ThreadInfo info = bean.getThreadInfo(id, 8);
            if (info == null || !looksRenderRelated(info)) continue;
            long value = bean.getThreadCpuTime(id);
            if (value > 0) total += value;
        }
        return total;
    }

    private static boolean looksRenderRelated(ThreadInfo info) {
        StringBuilder text = new StringBuilder(info.getThreadName().toLowerCase(Locale.ROOT));
        for (StackTraceElement frame : info.getStackTrace()) {
            text.append(' ').append(frame.getClassName().toLowerCase(Locale.ROOT));
            text.append(' ').append(frame.getMethodName().toLowerCase(Locale.ROOT));
        }

        String s = text.toString();
        return containsAny(s,
                "render", "renderer", "rendering", "graphics",
                "opengl", "vulkan", "blaze", "chunk", "terrain",
                "cull", "vertex", "buffer", "upload", "draw");
    }

    private static void writeSnapshot(ThreadMXBean bean, double totalBusy, double renderBusy) {
        List<ThreadSample> samples = new ArrayList<ThreadSample>();
        for (long id : bean.getAllThreadIds()) {
            ThreadInfo info = bean.getThreadInfo(id, 8);
            if (info == null) continue;
            long cpu = bean.getThreadCpuTime(id);
            if (cpu <= 0) continue;
            samples.add(new ThreadSample(info.getThreadName(), cpu, looksRenderRelated(info)));
        }

        samples.sort(Comparator.comparingLong((ThreadSample s) -> s.cpu).reversed());
        StringBuilder top = new StringBuilder();
        int limit = Math.min(8, samples.size());
        for (int i = 0; i < limit; i++) {
            if (i > 0) top.append('|');
            top.append(escape(samples.get(i).name));
            top.append(':').append(samples.get(i).renderRelated ? 'r' : 'o');
        }

        String json = "{"
                + "\"pid\":" + pid()
                + ",\"threads\":" + bean.getThreadCount()
                + ",\"cpu_busy\":" + number(totalBusy)
                + ",\"render_busy\":" + number(renderBusy)
                + ",\"top\":\"" + top + "\""
                + "}";

        Path target = new File(OUTPUT).toPath();
        Path parent = target.getParent();
        Path temp = target.resolveSibling(target.getFileName().toString() + ".tmp");
        try {
            if (parent != null) Files.createDirectories(parent);
            try (BufferedWriter writer = Files.newBufferedWriter(temp, StandardCharsets.UTF_8)) {
                writer.write(json);
            }
            try {
                Files.move(temp, target, StandardCopyOption.ATOMIC_MOVE,
                        StandardCopyOption.REPLACE_EXISTING);
            } catch (AtomicMoveNotSupportedException ignored) {
                Files.move(temp, target, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException ignored) {
            return;
        }
    }

    private static long pid() {
        try {
            String value = ManagementFactory.getRuntimeMXBean().getName();
            int at = value.indexOf('@');
            return Long.parseLong(at >= 0 ? value.substring(0, at) : value);
        } catch (Exception ignored) {
            return -1L;
        }
    }

    private static String number(double value) {
        return String.format(Locale.ROOT, "%.4f", clamp(value));
    }

    private static double clamp(double value) {
        return Math.max(0.0, Math.min(1.0, value));
    }

    private static boolean containsAny(String text, String... values) {
        for (String value : values) if (text.contains(value)) return true;
        return false;
    }

    private static String escape(String value) {
        StringBuilder escaped = new StringBuilder(value.length());
        for (int i = 0; i < value.length(); i++) {
            char character = value.charAt(i);
            switch (character) {
                case '\\': escaped.append("\\\\"); break;
                case '"': escaped.append("\\\""); break;
                case '\b': escaped.append("\\b"); break;
                case '\f': escaped.append("\\f"); break;
                case '\n': escaped.append("\\n"); break;
                case '\r': escaped.append("\\r"); break;
                case '\t': escaped.append("\\t"); break;
                default:
                    if (character < 0x20) {
                        escaped.append(String.format(Locale.ROOT, "\\u%04x", (int) character));
                    } else {
                        escaped.append(character);
                    }
            }
        }
        return escaped.toString();
    }

    private static final class ThreadSample {
        final String name;
        final long cpu;
        final boolean renderRelated;
        ThreadSample(String name, long cpu, boolean renderRelated) {
            this.name = name;
            this.cpu = cpu;
            this.renderRelated = renderRelated;
        }
    }
}