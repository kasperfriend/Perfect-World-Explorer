# Architect

Architect is an optional world-authoring workspace registered through the normal module package system.

## Entering and leaving

Open the main menu and choose ARCHITECT | WORLD EDITOR. The original HUD and viewer panels are hidden while the existing 3D world renderer remains the viewport. Use the red Turn off Architect control in the top toolbar to return to the normal viewer.

## Workspace

- World (left by default): choose an empty project, any readable client map, or a saved Architect project. Client map changes use the viewer's existing world handoff and reopen Architect in the destination map.
- Client assets (right by default): browse the client folder tree lazily, expand PCK archive indexes on demand, and right-click folders/files/archive entries to pin them to the quick bar.
- Scene objects: select loaded models, trees, scenery and effects; multi-select and duplicate them into the editable project layer.
- Transform: edit staged custom placements' position, rotation and scale.
- Terrain tools: record raise/lower/flatten/smooth/paint operations with radius, strength and terrain/object/smart snapping settings.
- Quick bar (bottom): select pinned assets quickly.

Panels are movable and can be repositioned to the left, right, bottom or floating placement. Directory and PCK indexes are cached until Refresh files is used.

## Project storage

Projects are saved as portable UTF-8 .pwa files under the user's application data directory:

- Windows: %LOCALAPPDATA%/PerfectWorldExplorer/Architect/Worlds
- Linux: $XDG_DATA_HOME/PerfectWorldExplorer/Architect/Worlds, or ~/.local/share/PerfectWorldExplorer/Architect/Worlds

The project format stores a source map ID, asset references, object transforms and terrain-brush operations. It is versioned and validates numeric values and file size on load.

## Current implementation boundary

This is the initial editor and project-format milestone. Staged object placements and terrain-brush operations are persisted, but are not yet drawn into the live world renderer or applied to native terrain height data. Existing client scene objects are selectable/read-only; duplicating one creates an editable project placement. Native map export is deliberately disabled until validated ECWLD, EC-BSD and terrain writers exist. PCK archive paths can be browsed and pinned, but their assets are not yet dynamically instantiated in the renderer.