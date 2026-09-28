#!/bin/sh
# Build a SYNTHETIC rice shaped like the target machine's (AGENT_PROMPT §1),
# under the home directory given as $1 (default: $HOME). It copies the
# layout, never the user's files: every file here is a short invented
# placeholder.
#
#   ~/.local/share/caelestia   a git repo: committed placeholders, then some
#                              modified and some untracked (the "stable state")
#   ~/.config/{hypr fish foot btop fastfetch uwsm spicetify}  7 DIRECTORY links into it
#   ~/.config/starship.toml, ~/.config/codium-flags.conf    2 FILE links into it
#   ~/.config/{kitty,nvim}     plain user directories with dummy files
#   ~/.local/bin/hypr-session  a fake of the user's session script (never run)
#
# Run inside the acceptance container only.
set -eu
H=${1:-$HOME}
C=$H/.local/share/caelestia
case "$H" in /home/rice*|/home/rice2*) ;; *) echo "setup-rice.sh: refusing home $H outside the container's users" >&2; exit 2 ;; esac
[ ! -e "$C" ] || { echo "setup-rice.sh: $C already exists" >&2; exit 2; }

mkdir -p "$C" "$H/.config" "$H/.local/bin"
cd "$C"
git init -q -b main .

w() { mkdir -p "$(dirname "$1")"; printf '%s\n' "$2" > "$1"; }

w hypr/hyprland.conf '# placeholder hyprland.conf (synthetic)
source = ./scheme/current.conf
source = ./hyprland/general.conf
exec = cp ./scheme/default.conf ./scheme/current.conf
exec-once = placeholder-daemon --start
general {
    gaps_in = 5
}'
w hypr/hyprland/general.conf '# placeholder
decoration {
    rounding = 10
}'
w hypr/scheme/default.conf '# placeholder scheme
$accent = rgb(aabbcc)'
w hypr/scripts/configs.fish '# placeholder script'
w fish/config.fish '# placeholder fish config
set -g fish_greeting ""'
w fish/functions/hello.fish 'function hello; echo hi; end'
w foot/foot.ini '# placeholder
[main]
font=monospace:size=11'
w btop/btop.conf '# placeholder
color_theme = "caelestia"'
w btop/themes/caelestia.theme '# placeholder theme'
w fastfetch/config.jsonc '{ "placeholder": true }'
w uwsm/env '# placeholder uwsm env
export PLACEHOLDER=1'
w spicetify/Themes/caelestia/color.ini '; placeholder'
w starship.toml '# placeholder starship
add_newline = false'
w vscode/flags.conf '--placeholder-flag'
w zen/userChrome.css '/* placeholder */'
w install.fish '# placeholder installer: ricepilot must never run this'
w README.md 'placeholder caelestia clone (synthetic, acceptance sandbox)'
w .gitignore '*.log
fish/fish_variables'

git add -A
git -c user.name=acceptance -c user.email=acceptance@invalid commit -q -m 'synthetic caelestia placeholder'

# The stable state: some modified, some untracked (the real one has 9 + ~20).
printf '# local edit\n' >> hypr/hyprland/general.conf
printf '# local edit\n' >> fish/config.fish
printf 'font=monospace:size=12\n' >> foot/foot.ini
w hypr/scheme/current.conf '# generated placeholder scheme'
w hypr/sessions/.keep ''
w fish/fish_variables '# volatile placeholder'
w btop/themes/local.theme '# untracked placeholder'
w fastfetch/extra.jsonc '{}'
w spicetify/Themes/caelestia/user.css '/* untracked */'
w notes-untracked.txt 'untracked placeholder'
# a mode-600 file (init should propose it as volatile) and an absolute
# in-tree symlink (init should report it)
w hypr/secret.conf '# owner-only placeholder'
chmod 600 hypr/secret.conf
ln -s "$C/zen/userChrome.css" zen/userChrome-abs.css

cd "$H/.config"
for d in hypr fish foot btop fastfetch uwsm spicetify; do ln -s "$C/$d" "$d"; done
ln -s "$C/starship.toml" starship.toml
ln -s "$C/vscode/flags.conf" codium-flags.conf

mkdir -p kitty nvim/lua
w kitty/kitty.conf '# placeholder kitty
font_size 11'
w kitty/theme.conf '# placeholder'
w nvim/init.lua '-- placeholder'
w nvim/lua/plugins.lua '-- placeholder'

w "$H/.local/bin/hypr-session" '#!/bin/sh
# FAKE hypr-session placeholder (the real one does mkdir -p ~/.config/hypr/sessions)
SESSION_DIR="$HOME/.config/hypr/sessions"
mkdir -p "$SESSION_DIR"'
chmod 755 "$H/.local/bin/hypr-session"

echo "setup-rice.sh: synthetic rice built under $H"
