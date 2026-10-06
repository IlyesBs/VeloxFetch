# Lists (or closes) visible top-level windows. Used by real-browser.mjs to see whether the browser
# shows its own "Save as" window. Usage: windows-list.ps1            -> one "handle|title" per line
#                                        windows-list.ps1 -Close <h> -> ask window <h> to close
param([long]$Close = 0)
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class VeloxWin {
  public delegate bool P(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(P p, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  public static string[] Titles() {
    var r = new List<string>();
    EnumWindows((h, l) => {
      if (IsWindowVisible(h)) { var s = new StringBuilder(300); GetWindowText(h, s, 300); if (s.Length > 0) r.Add(h.ToInt64() + "|" + s); }
      return true;
    }, IntPtr.Zero);
    return r.ToArray();
  }
  public static void CloseWindow(long h) { PostMessage(new IntPtr(h), 0x0010, IntPtr.Zero, IntPtr.Zero); }
}
"@
if ($Close -ne 0) { [VeloxWin]::CloseWindow($Close) } else { [VeloxWin]::Titles() }
