// Batch-decompile functions listed in a file (one hex address per line, '#' comments allowed)
// and write the decompiler output to a text file. Read-only: never modifies the program.
// Usage (analyzeHeadless ... -noanalysis -postScript DecompileList.java <list> <out>)
// @category OpenMaker.RE

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileOptions;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

import java.io.BufferedReader;
import java.io.FileReader;
import java.io.PrintWriter;

public class DecompileList extends GhidraScript {
    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        DecompInterface ifc = new DecompInterface();
        ifc.setOptions(new DecompileOptions());
        ifc.openProgram(currentProgram);
        try (BufferedReader in = new BufferedReader(new FileReader(args[0]));
             PrintWriter out = new PrintWriter(args[1], "UTF-8")) {
            String line;
            while ((line = in.readLine()) != null) {
                line = line.trim();
                if (line.isEmpty() || line.startsWith("#")) {
                    continue;
                }
                String hex = line.split("\\s+")[0];
                Address a = toAddr(Long.parseUnsignedLong(hex, 16));
                Function f = getFunctionContaining(a);
                if (f == null) {
                    out.println("//// " + hex + " : no function");
                    continue;
                }
                DecompileResults r = ifc.decompileFunction(f, 180, monitor);
                out.println("//// " + hex + " : " + f.getName(true));
                if (r != null && r.decompileCompleted()) {
                    out.println(r.getDecompiledFunction().getC());
                } else {
                    out.println("// decompile failed: " + (r == null ? "null" : r.getErrorMessage()));
                }
                out.flush();
            }
        } finally {
            ifc.dispose();
        }
    }
}
