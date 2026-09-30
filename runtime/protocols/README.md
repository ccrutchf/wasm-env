Wayland protocol descriptions and a keymap for the runtime's own compositor
(runtime/src/wayland/).

- `wayland.xml`: the core protocol, from Wayland 1.25 (MIT, see its
  `<copyright>`).
- `xdg-shell.xml`, `viewporter.xml` (stable), `xdg-decoration-unstable-v1.xml`,
  `xdg-output-unstable-v1.xml` (unstable): from wayland-protocols 1.49 (MIT,
  see each file's `<copyright>`).
- `keymap-us.xkb`: the US layout compiled by xkbcommon 1.13
  (`xkbcli compile-keymap --rules evdev --model pc105 --layout us`), from
  xkeyboard-config's data (MIT-style licences). The compositor sends it to
  clients as-is, so hosts need no xkbcommon.
