# alttabway

Finally, an alt-tab window switcher with actual window previews. Only Hyprland is supported at the moment.

![Preview](https://cdn.jsdelivr.net/gh/radiantly/alttabway/.github/preview.png)

## Usage

You will need [cargo](https://doc.rust-lang.org/stable/cargo/getting-started/installation.html) installed.

```sh
cargo install alttabway
```

alttabway is now installed! Follow compositor specific instructions to start the daemon and bind the hotkey.

- #### Hyprland

  Add the following lines to your `~/.config/hypr/hyprland.conf`

  ```toml
  exec-once = alttabway daemon &
  binde = ALT, Tab, exec, alttabway show --next
  binde = ALT SHIFT, Tab, exec, alttabway show --previous

  # Optional - set this if you have blur enabled on Hyprland
  layerrule = blur on, ignore_alpha 0, match:namespace ^alttabway$
  ```

  <details>
  <summary>Configuration options</summary>

  ```toml
  # Activate using Ctrl+Super+Tab
  binde = CTRL SUPER, Tab, exec, alttabway show --next --modifiers-held ctrl,super
  binde = CTRL SUPER SHIFT, Tab, exec, alttabway show --previous --modifiers-held ctrl,super
  ```

  </details>

## FAQ

#### The window preview is sometimes missing. Why?

alttabway uses wlr-screencopy-unstable-v1 to generate a preview of your active window. Sometimes, it is unable to generate this preview if you open a window and navigate away from it too quickly.

#### Sometimes there's a delay between holding the alt-tab hotkey and the window showing up

Window preview resizing runs on the main thread and needs to move to a background thread. Should be fixed soon.

#### Can I use a different hotkey combination?

No

#### Please support $COMPOSITOR

alttabway only provides support for Hyprland (Sway and Niri coming in the near future). Open an issue if you'd like support for your compositor. Typically the compositor should implement the following protocols.

- wlr-foreign-toplevel-management-unstable-v1 for the list of top level windows and to activate one
- wlr-screencopy-unstable-v1 to take a capture of a region on screen.
  - Window positions/dimensions are required as well, typically via ipc.

#### The alttabway window doesn't show up

Try setting `render_backend` to `Vulkan`, `Gl` or `Software` in the configuration.

#### Compile fails with "try setting PKG_CONFIG_PATH to the directory containing wayland-client.pc/xkbcommon.pc"

Some required dependencies are missing. On debian based distros, run `apt install libwayland-dev libxkbcommon-dev`
