// Decompile a list of functions from an already-analyzed program and write
// one text file per function. Read-only use against a pre-analyzed project.
//
// Script args: <list file> <output dir>
//   list file: one entry per line, "<hex address> [label]"; '#' comments allowed.
//   output dir: receives <label or address>.c files.
//
// Run with analyzeHeadless <projdir> <projname> -process <program> -readOnly
//   -noanalysis -scriptPath tools/ghidra -postScript DecompileList.java <list> <out>
//@category OpenMaker

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileOptions;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

import java.io.File;
import java.io.PrintWriter;
import java.nio.file.Files;
import java.util.List;

public class DecompileList extends GhidraScript {
    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        List<String> lines = Files.readAllLines(new File(args[0]).toPath());
        File outDir = new File(args[1]);
        outDir.mkdirs();
        DecompInterface ifc = new DecompInterface();
        DecompileOptions opts = new DecompileOptions();
        ifc.setOptions(opts);
        ifc.openProgram(currentProgram);
        for (String line : lines) {
            line = line.trim();
            if (line.isEmpty() || line.startsWith("#")) {
                continue;
            }
            String[] parts = line.split("\\s+", 2);
            Address addr = toAddr(parts[0]);
            String label = parts.length > 1 ? parts[1].replaceAll("[^A-Za-z0-9_.-]", "_") : parts[0];
            Function f = getFunctionAt(addr);
            if (f == null) {
                f = getFunctionContaining(addr);
            }
            File out = new File(outDir, label + ".c");
            try (PrintWriter pw = new PrintWriter(out, "UTF-8")) {
                if (f == null) {
                    pw.println("// no function at " + addr);
                    continue;
                }
                DecompileResults res = ifc.decompileFunction(f, 120, monitor);
                pw.println("// " + f.getName(true) + " @ " + f.getEntryPoint());
                if (res != null && res.decompileCompleted()) {
                    pw.println(res.getDecompiledFunction().getC());
                } else {
                    pw.println("// decompile failed: " + (res == null ? "null" : res.getErrorMessage()));
                }
            }
            println("decompiled " + label);
        }
        ifc.dispose();
    }
}
