# Optional Grasshopper binary archive converter

This standalone add-on converts genuine Grasshopper `.gh` binary archives to
and from `.ghx` XML. It uses McNeel's `GH_IO.dll` archive library. Rhino,
Grasshopper and a commercial Rhino license are not required to run the
converter. It never opens a Grasshopper document, loads plug-ins or evaluates
Python/C# scripts. Supported component evaluation happens inside MantisCAD
after import; unsupported components and dependent nodes are reported.

The base MantisCAD app reads and writes supported GHX graphs without this
add-on. Extract a release archive and run `install.cmd` (Windows) or
`./install.sh` (Linux). This installs for the current user without administrator
rights, and MantisCAD discovers it automatically. The separate CAD compatibility
pack can be installed alongside it. Alternatively, copy this add-on beside the app at
`compat/grasshopper/mantis-gh-io.exe` (Windows) or
`compat/grasshopper/mantis-gh-io` (Linux), or set `MANTIS_GH_CONVERTER` to the
executable. Keep the other add-on files together in the same folder.

```
mantis-gh-io decode input.gh > output.ghx
mantis-gh-io encode output.gh < input.ghx
```

Encode refuses to overwrite an existing file. Decode rejects input files over
64 MiB and generated XML over 64 MiB. The application also bounds process
runtime and output size. File conversion is separate from the scope of the
GHX component translator: proprietary plug-ins, arbitrary scripts, referenced
Rhino objects, and full data-tree processing are not implemented.

Linux needs `libgdiplus` and its standard Cairo/Pango dependencies because
Grasshopper archives can contain thumbnail images. The add-on launcher also
looks in its own `lib` directory; release packages may bundle these libraries.
Windows uses its built-in GDI+ support. The self-contained .NET runtime is
included in release add-ons, so users do not need to install .NET.

Build with a .NET 8 SDK:

```
python3 build.py --rid win-x64 --output build/grasshopper-win-x64
python3 build.py --rid linux-x64 --output build/grasshopper-linux-x64 --linux-libs extracted-gdi-packages
```

The build fetches the official McNeel Grasshopper NuGet package, references
only GH_IO, and restores Microsoft's System.Drawing compatibility library.
See NOTICE.md for provenance and third-party notices.
The optional `--linux-libs` directory is an extracted Debian/Ubuntu package
root containing the GDI shared libraries and package copyright files. Without
it, the target system must provide libgdiplus and its dependencies.

API references:

- https://developer.rhino3d.com/api/grasshopper/html/N_GH_IO.htm
- https://developer.rhino3d.com/api/grasshopper/html/M_GH_IO_Serialization_GH_Archive_ReadFromFile.htm
- https://developer.rhino3d.com/api/grasshopper/html/M_GH_IO_Serialization_GH_Archive_Serialize_Binary.htm
- https://www.nuget.org/packages/Grasshopper/8.34.26223.11001
