# Real terminal recording of aco 0.1.0 (charmbracelet/vhs).
# Run from the repo root after `cargo build --release`:  vhs release-run/demo.tape
Output release-run/demo.mp4

Set Shell "bash"
Set FontFamily "DejaVu Sans Mono"
Set FontSize 17
Set Width 1600
Set Height 960
Set Padding 18
Set Theme "Dracula"
Set TypingSpeed 45ms
Set WindowBar Colorful

Hide
Type "unset NO_COLOR; export COLORTERM=truecolor PATH=$PWD/target/release:$PATH PS1='\[\e[1;32m\]$\[\e[0m\] ' && rm -rf demo-out && clear"
Enter
Show

Sleep 800ms
Type "aco --json --out demo-out fixtures/bubble.py"
Sleep 500ms
Enter
Wait+Screen@60s /confirming · round [3-6]/
Sleep 300ms
Screenshot release-run/still-1-search.png
Wait+Screen@120s /q quit/
Sleep 3500ms
Screenshot release-run/still-2-bubble-summary.png
Sleep 1500ms
Type "q"
Sleep 2500ms

Type "clear && aco --out demo-out --mutate fixtures/stress/project/shop/inventory.py fixtures/stress/project/main.py"
Sleep 400ms
Enter
Wait+Screen@120s /q quit/
Sleep 3500ms
Screenshot release-run/still-4-project-mutate.png
Sleep 1000ms
Type "q"
Sleep 2500ms

Type "clear && aco --plain --out demo-out fixtures/stress/numpy_project/stats.py"
Sleep 400ms
Enter
Wait+Screen@120s /faster, confirmed|original kept/
Sleep 1000ms
Screenshot release-run/still-5-numpy-plain.png
Sleep 3500ms

Type "clear && aco --plain --out demo-out fixtures/slower.py"
Sleep 400ms
Enter
Wait+Screen@120s /original kept/
Sleep 1000ms
Screenshot release-run/still-3-slower-rejected.png
Sleep 4000ms
