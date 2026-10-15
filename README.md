<p align="center">
  <img src="docs/images/skating-crab.png" alt="Rust crab riding a skateboard" width="480">
</p>

# Skate 3 Rust Engine

A Rust and Bevy skating project built from Skate 3 reverse-engineering research.
Includes skating, tricks, grinds, offboard movement, difficulty settings and
`.skate` map support. Gameplay parity is still a work in progress.

## History

Before this rewrite existed, **dumbad** spent more than two years reverse
engineering Skate 3 and building the tools needed to understand and work with
it. That meant countless hours digging through undocumented file formats,
animation data, and game interaction systems, then testing those discoveries
in the original game. Much of that work is collected in
[DumbadsSkate3ModdingTools](https://github.com/Ethanw05/DumbadsSkate3ModdingTools),
including tools for custom maps, meshes, collision, challenges, and DLC.

That research laid the groundwork for this project. Chasm later worked on a
recompilation and a custom renderer based on dumbad's earlier renderer work,
before moving into the Rust/Bevy rewrite. The rewrite's development time tells
only part of the story: the knowledge and tools it relies on took years of
work to establish.

## AI usage

AI coding tools were used to develop this rewrite, but none of it would have
been possible without dumbad's extraordinary effort to reverse engineer the
original game. The AI had years of hard-earned research and working tools to
build on. Describing the project as simply “AI rewriting Skate 3” leaves out
the work that made it possible in the first place.

AI helped turn that knowledge into a new implementation; it does not replace
credit for discovering how the game works. This is still a work in progress,
and using original assets or showing working tricks does not mean every
system behaves exactly like the original.

## Play

[Download Experimental](https://github.com/SK8-ENGINE/skate-3-rust-engine/releases/tag/experimental).
Successful `main` builds replace this prerelease. Choose **Latest** in Updates
for experimental updates; **Stable** is the default.

Extract the Windows release ZIP and run `skate3rust.exe`. Select your Skate 3
Xbox 360 ISO, or select `default.xex` in an extracted game folder. Keep its
`data` folder alongside it. Setup prepares the skater, animations and all disc maps, then
starts University. The original scoring and session-marker HUD assets are also
exported automatically during setup. No Blender, Python or Rust installation is needed.
ISO extraction needs internet access. The first conversion can take a while.

Use an XInput controller to play. Escape opens graphics, difficulty and map
settings. Maps can be switched without restarting the game.

**Skate 3 assets are not included.** Your converted files stay in
the `data` folder beside your executable. Each freshly unpacked copy runs its
own setup; it does not adopt another installation. In-place updates refresh
only changed asset groups.

### Browser build

The **Deploy web to GitHub Pages** workflow publishes the browser client from
`main` (or on manual dispatch). In repository Settings > Pages, choose **GitHub
Actions** as the source. The page contains the client only; it does not upload
or include Skate 3 game data.

To have the page load your own converted game-data ZIP, set the repository
Actions variable `SKATE_CLIENT_PACK_URL` to its public HTTPS URL. The ZIP host
must allow browser CORS requests and byte-range reads. This URL is embedded in
the public Pages site, so do not put credentials or private tokens in it. With
the variable unset, the client can still be pointed at a game-data server or a
ZIP URL from the page's game-server settings.
Solo play does not require multiplayer. The game-server setting only tells the
browser where to read game files; no multiplayer session needs to be hosted or
joined. The browser client still needs your own converted game data.

## Build

Requires Windows, Rust with the MSVC toolchain, and LLVM installed in its default
location. Run `BUILD.bat` to build, then `PLAY.bat` to launch the test world.
`PLAY.bat` opens your saved map (University by default); use the in-game menu to switch maps, or drag a `.skate` file onto `PLAY.bat`. An XInput controller is required for gameplay;
Escape opens difficulty and graphics settings.

Development builds use a prepared asset set in `assets/private/` or the
installed asset directory. `scripts/Build-Release.ps1` builds the portable Windows
package and requires Python 3.13. GitHub Actions builds `main` automatically;
numbered releases are published separately.

Custom animations and climbing support remain available, but no custom clips
are shipped. The included format-demo map is original procedural content.

Implementation notes are in [`docs/`](docs/). Patched Bevy dependencies and
their licenses are in [`vendor/`](vendor/). This is an unofficial project,
not affiliated with EA.

## Advanced diagnostics

Windows builds support opt-in [performance timeline capture](docs/performance-tracing.md)
through the `--trace` CLI option, including optional GPU pass diagnostics.
