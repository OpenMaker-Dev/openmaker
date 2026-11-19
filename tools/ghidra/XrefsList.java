// List functions that reference each given address (read-only).
//
// Script args: <list file> <output file>
//   list file: one entry per line, "<hex address> [label]"; '#' comments allowed.
// Output: "<label> <target> <from address> <function entry> <function name>"
//@category OpenMaker

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;

import java.io.File;
import java.io.PrintWriter;
import java.nio.file.Files;
import java.util.List;

public class XrefsList extends GhidraScript {
    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        List<String> lines = Files.readAllLines(new File(args[0]).toPath());
        try (PrintWriter pw = new PrintWriter(new File(args[1]), "UTF-8")) {
            for (String line : lines) {
                line = line.trim();
                if (line.isEmpty() || line.startsWith("#")) {
                    continue;
                }
                String[] parts = line.split("\\s+", 2);
                Address addr = toAddr(parts[0]);
                String label = parts.length > 1 ? parts[1] : parts[0];
                int n = 0;
                for (Reference r : getReferencesTo(addr)) {
                    Function f = getFunctionContaining(r.getFromAddress());
                    pw.println(label + " " + addr + " " + r.getFromAddress() + " "
                        + (f == null ? "-" : f.getEntryPoint() + " " + f.getName(true)));
                    if (++n >= (args.length > 2 ? Integer.parseInt(args[2]) : 40)) {
                        pw.println(label + " ... truncated");
                        break;
                    }
                }
                if (n == 0) {
                    pw.println(label + " " + addr + " (no references)");
                }
            }
        }
    }
}
