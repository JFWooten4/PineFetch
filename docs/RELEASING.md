# Publishing

Run `npm run publish` from the PineFetch checkout. It synchronizes versions,
runs checks and the release build check, builds and signs the macOS app, creates
the DMG, publishes the GitHub release, and updates the Homebrew tap.

The tap defaults to `../homebrew-tap`. Set `HOMEBREW_TAP_DIR` to use another
checkout. Its current branch must match the corresponding branch on `origin`
before publishing starts.

Existing edits to `Casks/pinefetch.rb` and `README.md` are automatically committed
as `Prepare PineFetch release v<version>` before building the release. This keeps
those edits in Git history before the generated cask replaces the working copy.
The generated cask and README update are then committed as
`Update PineFetch cask to v<version>` and pushed after the release and, when Homebrew
is installed, its style check succeed. A preparation commit is pushed even if generation produces
no additional changes.

Other tap files are excluded, including changes already staged in Git. They keep
their existing working-tree and index state. Only the two named files are included;
the tap README is committed as a complete file.

If building, releasing or the style check fails, any preparation commit stays
local. Before retrying, synchronize that commit with the tap's remote branch. A
release that already exists still requires the usual manual recovery; the script
does not recreate existing releases.
