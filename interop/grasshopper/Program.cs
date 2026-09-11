using GH_IO.Serialization;
using System.Text;

// Archive conversion only: never instantiate a Grasshopper document or solve
// components. Python/C#/plug-in contents remain inert serialized data.
try
{
    Console.OutputEncoding = new UTF8Encoding(false);
    Console.InputEncoding = new UTF8Encoding(false);
    if (args.Length != 2 || (args[0] != "decode" && args[0] != "encode"))
        throw new ArgumentException("Usage: mantis-gh-io decode input.gh | encode output.gh (GHX on stdin)");
    const long limit = 64 * 1024 * 1024;
    var archive = new GH_Archive();
    if (args[0] == "decode")
    {
        var info = new FileInfo(args[1]);
        if (info.Length > limit) throw new IOException("Archive exceeds 64 MiB limit");
        if (!archive.ReadFromFile(info.FullName)) throw new IOException("GH_IO could not read this archive");
        string xml = archive.Serialize_Xml();
        if (Encoding.UTF8.GetByteCount(xml) > limit) throw new IOException("Decoded archive exceeds 64 MiB limit");
        Console.Write(xml);
    }
    else
    {
        var path = Path.GetFullPath(args[1]);
        if (!string.Equals(Path.GetExtension(path), ".gh", StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("Binary export path must end in .gh");
        if (File.Exists(path)) throw new IOException("Destination already exists; choose a new path");
        var buffer = new char[65536];
        var xml = new StringBuilder();
        int count;
        while ((count = Console.In.Read(buffer, 0, buffer.Length)) != 0)
        {
            xml.Append(buffer, 0, count);
            if (xml.Length > limit / 2) throw new IOException("Input XML exceeds limit");
        }
        if (!archive.Deserialize_Xml(xml.ToString())) throw new IOException("GH_IO rejected the GHX archive");
        // Serialize_Binary is the actual GH archive codec. Unlike the file
        // dialog helpers, it does not load Windows Forms on Linux.
        // A sibling temporary keeps a failed write away from the destination;
        // File.Move's non-overwrite mode closes the check/create race.
        var temporary = Path.Combine(Path.GetDirectoryName(path)!,
            $".mantis-temp-{Guid.NewGuid():N}.gh");
        try
        {
            var bytes = archive.Serialize_Binary();
            if (bytes.LongLength > limit) throw new IOException("Encoded archive exceeds limit");
            using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write))
            {
                output.Write(bytes);
                output.Flush(flushToDisk: true);
            }
            File.Move(temporary, path, overwrite: false);
        }
        finally
        {
            if (File.Exists(temporary)) File.Delete(temporary);
        }
    }
    return 0;
}
catch (Exception error)
{
    Console.Error.WriteLine($"Grasshopper archive conversion failed: {error.GetBaseException().Message}");
    return 1;
}
