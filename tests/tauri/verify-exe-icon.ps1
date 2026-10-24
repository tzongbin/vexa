param([Parameter(Mandatory = $true)][string]$ExecutablePath)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class VexaIconResource {
    [UnmanagedFunctionPointer(CallingConvention.Winapi)] public delegate bool EnumNamesCallback(IntPtr module, IntPtr type, IntPtr name, IntPtr parameter);
    static System.Collections.Generic.List<IntPtr> names = new System.Collections.Generic.List<IntPtr>();
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern IntPtr LoadLibraryEx(string file, IntPtr fileHandle, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll", EntryPoint = "EnumResourceNamesW", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool EnumResourceNames(IntPtr module, IntPtr type, EnumNamesCallback callback, IntPtr parameter);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern uint SizeofResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr LockResource(IntPtr resource);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool FreeLibrary(IntPtr module);
    [DllImport("user32.dll", SetLastError = true)] public static extern int LookupIconIdFromDirectoryEx(byte[] iconDirectory, bool isIcon, int width, int height, uint flags);
    [DllImport("user32.dll", SetLastError = true)] public static extern IntPtr CreateIconFromResourceEx(byte[] bits, uint size, bool isIcon, uint version, int width, int height, uint flags);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool DestroyIcon(IntPtr icon);
    static bool CollectName(IntPtr module, IntPtr type, IntPtr name, IntPtr parameter) { names.Add(name); return true; }
    public static IntPtr[] GroupNames(IntPtr module) { names.Clear(); EnumNamesCallback callback = CollectName; if (!EnumResourceNames(module, new IntPtr(14), callback, IntPtr.Zero)) throw new InvalidOperationException("No RT_GROUP_ICON resources found."); return names.ToArray(); }
    public static byte[] ReadResource(IntPtr module, IntPtr name, IntPtr type) {
        IntPtr resource = FindResource(module, name, type);
        if (resource == IntPtr.Zero) throw new InvalidOperationException("Required icon resource was not found.");
        uint size = SizeofResource(module, resource);
        IntPtr loaded = LoadResource(module, resource);
        IntPtr data = LockResource(loaded);
        if (size == 0 || data == IntPtr.Zero) throw new InvalidOperationException("Icon resource is empty.");
        byte[] bytes = new byte[(int)size]; Marshal.Copy(data, bytes, 0, (int)size); return bytes;
    }
}
'@
$resolved = (Resolve-Path -LiteralPath $ExecutablePath).Path
$module = [VexaIconResource]::LoadLibraryEx($resolved, [IntPtr]::Zero, 0x2)
if ($module -eq [IntPtr]::Zero) { throw "Cannot load EXE resource table: $resolved" }
try {
  $groupName = @([VexaIconResource]::GroupNames($module))[0]
  if ($null -eq $groupName) { throw 'No application icon group was found in the EXE.' }
  $group = [VexaIconResource]::ReadResource($module, $groupName, [IntPtr]::new(14))
  foreach ($size in @(16, 32)) {
    $iconId = [VexaIconResource]::LookupIconIdFromDirectoryEx($group, $true, $size, $size, 0)
    if ($iconId -le 0) { throw "EXE has no $size x $size icon resource." }
    $bits = [VexaIconResource]::ReadResource($module, [IntPtr]::new($iconId), [IntPtr]::new(3))
    $handle = [VexaIconResource]::CreateIconFromResourceEx($bits, [uint32]$bits.Length, $true, 0x00030000, $size, $size, 0)
    if ($handle -eq [IntPtr]::Zero) { throw "Cannot decode embedded $size x $size icon." }
    try {
      $icon = [System.Drawing.Icon]::FromHandle($handle)
      $bitmap = $icon.ToBitmap()
      try {
        if ($bitmap.Width -ne $size -or $bitmap.Height -ne $size) { throw "Embedded icon dimensions differ from $size x $size." }
        $bluePixels = 0
        for ($y = 0; $y -lt $bitmap.Height; $y++) {
          for ($x = 0; $x -lt $bitmap.Width; $x++) {
            $pixel = $bitmap.GetPixel($x, $y)
            if ($pixel.A -gt 0 -and $pixel.B -gt ($pixel.R * 1.2) -and $pixel.B -gt ($pixel.G * 1.08)) { $bluePixels++ }
          }
        }
        if ($bluePixels -lt (($size * $size) / 5)) { throw "Embedded $size x $size icon is not the Vexa blue icon." }
      } finally { $bitmap.Dispose() }
    } finally { [VexaIconResource]::DestroyIcon($handle) | Out-Null }
  }
  Write-Output "PASS EXE RT_GROUP_ICON: embedded blue 16x16 and 32x32 resources verified in $resolved"
} finally { [VexaIconResource]::FreeLibrary($module) | Out-Null }
