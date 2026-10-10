# 🐠 ThermalAcuarium

![ThermalAcuarium, realistic style](docs/realistic.png)

An aquarium that is your computer. Every real process is a fish, and the water is the machine.
Built with **Rust** (GTK4 + Libadwaita), themed from the active **Omarchy** theme.

| Hardware / process data | What you see |
| :--- | :--- |
| Process RAM | **Body size** (log scale: from a 1 MB plankton to a multi‑GB whale) |
| Process CPU | **Swim speed** and tail beat |
| Process disk writes | **Bubbles** from the fish's mouth |
| Same process name | A **shoal**: one species/colors per name, one fish per PID |
| CPU temperature | The water **boils**: bubbling from the sand, glow, foam, agitated fish |
| System RAM usage | Water gets **murky** when memory is nearly full |
| Load average | Strength of the **current** (marine snow) |
| Total disk writes | Air stone bubbling |
| Session processes (Hyprland, pipewire…) | **Decoration** (coral, anemones, rocks, seaweed) — never killable |

Fish are procedural (spine‑driven body, tail and fins), not flat sprites.

## Two visual styles

Press **`T`** (or click the style button in the header) to switch; the choice is remembered
(`~/.config/thermalacuarium/style`, or start with `--style real|stylized`).

- **Stylized** (default): pastel fish painted with your Omarchy theme colors.
- **Realistic**: a reef aquarium. 15 species with real anatomy and coloring (clownfish, blue and
  yellow tang, royal gramma, chromis, butterflyfish, angelfish, mandarin dragonet, anthias, cardinalfish,
  damselfish, pufferfish, wrasse, lionfish, grouper) with countershading, scales, fin rays and
  eyes with iris; deep water with light shafts and **caustics** on sand and rock, blurred reef wall,
  live rock, pebbled sand, glass reflections, a mirror‑like water surface, distance fog, and corals,
  kelp, sea fans, brain coral and bubble‑tip anemones for the protected processes. Boiling water
  turns amber with glassy bubbles.

The realistic style is naturalistic, not photographic. It **reads the active Omarchy theme** and adapts
to it: the water, light shafts, reef wall, rocks, sand tint, glass and air take their hue from the theme's
blue/cyan, dark themes give a night tank and light themes (catppuccin-latte, flexoki-light, white…) a
bright daytime tank, and fish get a faint tint of that water. Species keep their natural colors (a
clownfish is still orange). It follows theme changes live (it watches Omarchy's theme folder: run `omarchy theme set …` and the window and the tank recolor within a second, no restart); try other themes without
changing yours with `thermalacuarium --theme gruvbox`.

## Use

- **Hover** a fish: name, PID, RAM, CPU, disk writes. Its shoal lights up.
- **Click** a fish: confirm dialog → close that process (or the whole shoal). It sends `SIGTERM` and,
  if the process is still the same one after 3 s, `SIGKILL`. The fish floats belly‑up while it dies.
- `L` cycles name labels (big shoals / all / none) · `Space` pauses · `T` switches style · the ⓘ button in the header opens *About* (with the link to the author's X, [@theinaog](https://x.com/theinaog)).
- Processes that start swim in from the sides; processes that exit dissolve into bubbles.

Only processes of your own user are shown. The tank holds the 180 biggest.

## Install / run

Nothing needs to be installed: the app is one file that only needs **GTK 4 and libadwaita**
(already part of Omarchy).

### Omarchy plugin (recommended)

```bash
omarchy plugin add https://github.com/IAnMove/ThermalAcuarium --enable
```

That is all. The bar widget shows the CPU temperature (🐠, or 🫧 when the water would be boiling).
The first time you click it, the plugin downloads the app (~1.5 MB, from this repo's releases, with its
sha256 checked) into its own folder and opens it; after that it starts instantly. Click opens the
aquarium, right‑click opens another window. `omarchy plugin update` keeps it current (the launcher
fetches the binary of the new version when `manifest.json` changes).

Move the widget with `omarchy bar move io.github.ianmove.thermalacuarium --section left|center|right`.

### Portable (no installation, no plugin)

```bash
curl -L -o thermalacuarium https://github.com/IAnMove/ThermalAcuarium/releases/latest/download/thermalacuarium-x86_64-linux
chmod +x thermalacuarium && ./thermalacuarium
```

or from a clone, with the same launcher the plugin uses (downloads the binary, or builds it with
`cargo` if it cannot; `THERMALACUARIUM_BUILD=1` forces building from source):

```bash
git clone https://github.com/IAnMove/ThermalAcuarium && ./ThermalAcuarium/thermalacuarium.sh
```

It writes nothing outside `~/.config/thermalacuarium/style` (only if you switch style).

### Installed (PATH + app menu entry)

```bash
./install.sh            # builds, installs ~/.local/bin/thermalacuarium and a desktop entry
./install.sh --plugin   # ...and also adds the Omarchy bar widget
```

It is a normal (tiled) window; no Hyprland rules needed.

## Options

```text
thermalacuarium --temp 95          # force the water temperature to preview the boil
THERMALACUARIUM_LANG=es thermalacuarium
thermalacuarium --snapshot out.png [--style real] [--size 1180x680] [--seconds 8] [--temp 99] [--demo] [--dying 3] [--names 1] [--hover 600,300] [--bench]
thermalacuarium --snapshot gallery.png --gallery --style real --len 210   # all species
thermalacuarium --theme nord       # preview another Omarchy theme (or THERMALACUARIUM_THEME=nord)
thermalacuarium --new-instance     # open another window without activating the running one
```

`--snapshot` renders the tank to a PNG with Cairo (no window), handy for tweaking the look.

## How it stays light

Animation is capped at ~30 FPS and stops when the window is hidden. Each fish variant is painted once
with Cairo into a sprite and GTK composes it on the GPU (transforms, gradients and bubbles are GSK
nodes), instead of re‑rasterizing the whole tank every frame.

## Credits

Telemetry code (`/proc`, `/sys/class/hwmon`) and theming come from ThermalWatch.

## Screenshots

| Boiling water (realistic) | Stylized | Species |
| :--- | :--- | :--- |
| ![](docs/boiling.png) | ![](docs/stylized.png) | ![](docs/species.png) |

(Rendered with `--snapshot --demo`.)
