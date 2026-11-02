# Real terminal recording of scripts/install.sh installing the published
# v0.1.0 release into a temporary directory (charmbracelet/vhs).
# Run from the repo root:  vhs release-run/install.tape
Output release-run/install.mp4
Output release-run/install.gif

Set Shell "bash"
Set FontFamily "DejaVu Sans Mono"
Set FontSize 17
Set Width 1300
Set Height 720
Set Padding 18
Set Theme "Dracula"
Set TypingSpeed 40ms
Set WindowBar Colorful

Hide
Type "unset NO_COLOR; export COLORTERM=truecolor PS1='\[\e[1;32m\]$\[\e[0m\] ' && rm -rf /tmp/aco-demo-install && clear"
Enter
Show

Sleep 700ms
Type "curl -fsSL https://raw.githubusercontent.com/ayaan-cs/aco/main/scripts/install.sh | sh -s -- --dir /tmp/aco-demo-install"
Sleep 500ms
Enter
Wait+Screen@120s /uninstall/
Sleep 1200ms
Screenshot release-run/still-install.png
Sleep 2500ms
Type "/tmp/aco-demo-install/aco --version"
Enter
Sleep 2500ms
