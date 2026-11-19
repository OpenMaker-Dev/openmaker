on run argv
    set ownedPID to (item 1 of argv) as integer
    set stage to item 2 of argv
    tell application "System Events"
        set owned to first application process whose unix id is ownedPID
        tell owned
            set frontmost to true
            if name of window 1 does not contain "compiled" then error "Unexpected window; refusing keyboard automation"
            if stage is "workspace" then
                keystroke "s" using {command down, shift down}
            else if stage is "new" then
                if name of window 1 does not contain "Script Workspace" then error "Not in Script Workspace"
                keystroke "n" using command down
            else if stage is "name" then
                keystroke "OracleCompileGenerated"
                key code 36
            else if stage is "paste" then
                keystroke "v" using command down
            else if stage is "save" then
                keystroke "s" using command down
            end if
        end tell
    end tell
end run
