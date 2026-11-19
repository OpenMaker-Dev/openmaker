// Decompile a list of functions from a program imported with -noanalysis.
// Each function is disassembled and created on demand; switch tables the
// decompiler recovers are added as flows and the function is redone, so a
// large dispatcher can be read without a full auto-analysis of the library.
//
// Script args: <list file> <output dir> [decompile timeout seconds, default 120]
//   list file: one entry per line, "<hex address> [label]"; '#' comments allowed.
//   output dir: receives <label or address>.c files.
//
// Run with analyzeHeadless <projdir> <projname> -process <program>
//   -noanalysis -scriptPath tools/ghidra -postScript DecompileOnDemand.java <list> <out>
// (not -readOnly: created functions are kept in the scratch project copy).
//@category OpenMaker

import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileOptions;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSet;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.pcode.HighFunction;
import ghidra.program.model.pcode.JumpTable;
import ghidra.program.model.symbol.RefType;
import ghidra.program.model.symbol.SourceType;

import java.io.File;
import java.io.PrintWriter;
import java.nio.file.Files;
import java.util.List;

public class DecompileOnDemand extends GhidraScript {
    private Function ensureFunction(Address addr) {
        Function f = getFunctionAt(addr);
        if (f != null) {
            return f;
        }
        if (getInstructionAt(addr) == null) {
            new DisassembleCommand(addr, null, true).applyTo(currentProgram, monitor);
        }
        new CreateFunctionCmd(addr).applyTo(currentProgram, monitor);
        return getFunctionAt(addr);
    }

    // Adds recovered jump-table targets as computed-jump flows; returns true
    // when anything new was disassembled.
    private boolean addJumpTables(Function f, HighFunction hf) throws Exception {
        boolean changed = false;
        if (hf == null) {
            return false;
        }
        for (JumpTable jt : hf.getJumpTables()) {
            Address sw = jt.getSwitchAddress();
            Instruction ins = getInstructionAt(sw);
            for (Address target : jt.getCases()) {
                if (ins != null) {
                    ins.addOperandReference(0, target, RefType.COMPUTED_JUMP, SourceType.ANALYSIS);
                }
                if (getInstructionAt(target) == null) {
                    new DisassembleCommand(target, null, true).applyTo(currentProgram, monitor);
                    changed = true;
                }
            }
        }
        if (changed) {
            CreateFunctionCmd.fixupFunctionBody(currentProgram, f, monitor);
        }
        return changed;
    }

    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        List<String> lines = Files.readAllLines(new File(args[0]).toPath());
        File outDir = new File(args[1]);
        int timeoutSecs = args.length > 2 ? Integer.parseInt(args[2]) : 120;
        outDir.mkdirs();
        DecompInterface ifc = new DecompInterface();
        DecompileOptions opts = new DecompileOptions();
        opts.setMaxPayloadMBytes(512); // large dispatchers exceed the 50 MB default
        ifc.setOptions(opts);
        ifc.openProgram(currentProgram);
        println("image base " + currentProgram.getImageBase());
        for (String line : lines) {
            line = line.trim();
            if (line.isEmpty() || line.startsWith("#")) {
                continue;
            }
            String[] parts = line.split("\\s+", 2);
            // "<hex>" is a file virtual address (the image base is added);
            // anything else is an exact (mangled) symbol name.
            Address addr;
            if (parts[0].matches("(0x)?[0-9a-fA-F]+")) {
                addr = currentProgram.getImageBase().add(Long.parseLong(parts[0].replaceFirst("^0x", ""), 16));
            } else {
                List<ghidra.program.model.symbol.Symbol> syms = getSymbols(parts[0], null);
                if (syms.isEmpty()) {
                    println("no symbol " + parts[0]);
                    continue;
                }
                addr = syms.get(0).getAddress();
            }
            String label = parts.length > 1 ? parts[1].replaceAll("[^A-Za-z0-9_.-]", "_") : parts[0];
            if (label.length() > 120) {
                label = label.substring(0, 120);
            }
            File out = new File(outDir, label + ".c");
            try (PrintWriter pw = new PrintWriter(out, "UTF-8")) {
                Function f = ensureFunction(addr);
                if (f == null) {
                    pw.println("// no function at " + addr);
                    continue;
                }
                DecompileResults res = null;
                for (int pass = 0; pass < 4; pass++) {
                    res = ifc.decompileFunction(f, timeoutSecs, monitor);
                    if (res == null || !res.decompileCompleted()
                            || !addJumpTables(f, res.getHighFunction())) {
                        break;
                    }
                }
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
