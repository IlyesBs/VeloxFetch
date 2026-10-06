// Takes a real screen capture of the app window (what the user sees, Mica included; a WebView2
// screenshot through the debug port can't show the window material). Writes docs/screenshots/<name>.png.
//
//   node tests/e2e/shot-window.mjs [path-to-velox-desktop.exe]
//
// Windows only. Starts its own app with private data, like every e2e script.

import { execFileSync } from "node:child_process";
import { createHarness, exe, path, sleep, startFileServer } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(96 * MB);
const h = createHarness({ port: 9365, appPath: APP });

/** Bring the window forward and copy its rectangle from the screen. */
function capture(pid, file) {
  const ps = `
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Rt, B; }
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out R r, int s);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
}
"@
Add-Type -AssemblyName System.Drawing
$h = (Get-Process -Id ${pid}).MainWindowHandle
[W]::SetWindowPos($h, [IntPtr]::Zero, 80, 60, 1240, 860, 0x0040) | Out-Null
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 900
$r = New-Object W+R
[W]::DwmGetWindowAttribute($h, 9, [ref]$r, 16) | Out-Null
$bmp = New-Object System.Drawing.Bitmap ($r.Rt - $r.L), ($r.B - $r.T)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$bmp.Save("${file.replace(/\\/g, "\\\\")}")
`;
  execFileSync("powershell", ["-NoProfile", "-Command", ps], { stdio: "inherit" });
}

try {
  await h.launch();
  await sleep(1500);
  await h.invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 3 * MB } });
  for (const name of ["Fedora-Workstation-41.iso", "blender-4.3-windows-x64.zip"]) await h.invoke("create_download", { request: { url: `${srv.base}/${name}` } });
  for (const [themeName, file] of [["dark", "window-mica-dark.png"], ["light", "window-mica-light.png"]]) {
    // through Settings, so the native frame (Mica tint) switches too, as for a user
    await h.page(`(document.querySelector('[data-testid=nav-settings]').click(), true)`);
    await sleep(300);
    await h.page(`(document.querySelector('[data-testid=theme-${themeName}]').click(), document.querySelector('[data-testid=nav-downloads]').click(), true)`);
    await sleep(2500);
    capture(h.app.pid, path.join(SHOTS, file));
    console.log("saved", file);
  }
} finally {
  srv.close();
  await h.cleanup();
}
