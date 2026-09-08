# Repository browser

**6 Repos** presents the existing repository inventory as
`repository → installed skills → preview`. It uses the same records as Search
and the `R` repository picker. It does not scan a separate legacy `projects/`
directory, import a second copy, or change installation and deployment behavior.

Start the TUI with your existing skills root. Click **6 Repos**, or leave the
Search input with `Tab` and press `6`.

- `↑/↓` or `j/k`: select a repository or skill.
- `Enter/→`: enter a repository, then focus its skill preview.
- `PageUp/PageDown` or the mouse wheel: navigate or scroll the preview.
- `Esc/←/Backspace`: leave the preview, return to repositories, then Search.
- Double-click a repository to open it; click the preview to scroll it.
- `Ctrl+r`: refresh the inventory.

Repository rows show installed skill counts; previews show repository source,
branch, and the existing skill details. Registered repositories with no installed
skills remain visible, and discovered repository skills without a source record
are identified as unregistered. Skills outside repository directories appear in a separate Local skills group.
Unlike Tags (topic labels) and Presets (deployment combinations), repository
groups are derived automatically from storage identities.
