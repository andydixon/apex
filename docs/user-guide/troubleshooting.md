# Troubleshooting

**Where are the logs?** Apex logs to stderr. Run `apex --log debug` (or
`APEX_LOG=debug apex`) from a terminal; `trace` includes every language
server message. There is no telemetry and no log file is written unless
you redirect one.

**The window is blank or does not open.** Apex renders with wgpu. Set
`WGPU_BACKEND=gl` to try OpenGL, or `WGPU_BACKEND=vulkan` with a software
Vulkan driver (lavapipe) on machines without a GPU; the start-up error names
the backend that failed. On Wayland without a compositor extension for
decorations, `WINIT_UNIX_BACKEND=x11` forces X11.

**Settings dialog on start-up.** The dialog lists the keys in `settings.toml`
or the project file that were unknown, mistyped or out of range; those keys
were ignored and everything else applied. **Tools > Settings** shows the
valid keys.

**"No dictionary found".** Install a Hunspell dictionary (`hunspell-en-gb`
on Debian/Ubuntu, `/Library/Spelling` on macOS) or put a word list at
`<config>/dictionaries/<lang>.dic`, then toggle spell check again.

**A language server does not start.** Check that its program is on the
`PATH` of the shell that launched Apex (desktop launchers often have a
shorter `PATH`); the status message shows the command tried and the last
lines of its stderr. Configure the full path under `[lsp_servers]`.

**A build task is greyed out.** Tasks from a project file run only after
**Project > Trust Workspace**.

**Fonts show boxes.** The bundled font covers Latin, Greek and Cyrillic;
set `font_family` to an installed font with the glyphs you need (for
example Noto Sans Mono CJK). Icons in the interface are drawn, not font
glyphs, so they are unaffected.

**Keys do nothing in the editor.** Vim mode may be on (the status bar says
`-- NORMAL --`): press `i` to insert or turn it off under **Tools**. A
pending chord such as `Ctrl+K` is shown in the status bar; `Escape`
cancels it.

**Recovering work after a crash.** Start Apex; it offers the snapshots it
kept. They live in the data directory under `recovery/` if you need them by
hand. Sessions are in `session.json` next to them.

**Resetting everything.** Delete the configuration and data directories
listed in **Help > About**.

**Reporting a problem.** Include the version (`apex --version`), platform,
the `--log debug` output around the problem and, for crashes, the
`RUST_BACKTRACE=1` output. Crash reports are never sent automatically.
