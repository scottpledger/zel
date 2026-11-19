import com.google.devtools.build.lib.bazel.repository.decompressor.PatchUtil;
import com.google.devtools.build.lib.vfs.*;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.List;

/**
 * Applies a patch with Bazel's real `PatchUtil`.
 * Usage: PatchOracle <patch> <strip> <dir>      or      PatchOracle --batch <listfile> <resultfile>
 * (the list file has one `patch<TAB>strip<TAB>dir` per line; results are written one line each).
 */
public class PatchOracle {
  static String run(FileSystem fs, String patch, String strip, String dir) {
    try {
      PatchUtil.apply(fs.getPath(patch), Integer.parseInt(strip), fs.getPath(dir));
      return "OK";
    } catch (Throwable e) {
      return "ERR " + e.getClass().getSimpleName() + ": " + String.valueOf(e.getMessage()).replace("\n", "\\n");
    }
  }

  public static void main(String[] a) throws Exception {
    FileSystem fs = new JavaIoFileSystem(DigestHashFunction.SHA256);
    if (a[0].equals("--batch")) {
      List<String> lines = Files.readAllLines(Paths.get(a[1]));
      StringBuilder out = new StringBuilder();
      for (String l : lines) {
        String[] p = l.split("\t");
        out.append(run(fs, p[0], p[1], p[2]).replace("\r", "\\r")).append("\n");
      }
      Files.writeString(Paths.get(a[2]), out.toString());
    } else {
      System.out.println(run(fs, a[0], a[1], a[2]));
    }
  }
}
