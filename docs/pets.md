# Pet Pack Format (pet pack)

Pet characters live in `~/.opencapx/pets/<slug>/`, one pack per directory, and the manifest file is always called `pet.json`.

The manifest fields follow the existing pet-pack convention (`id` / `displayName` / `description` / `spritesheetPath`),
so ready-made community 2D packs can be copied straight into the directory and used.

## Two Types

| | 2D (default) | 3D |
|---|---|---|
| Assets | Sprite sheet (`.png` / `.webp`) | glTF model (`.glb` / `.gltf`) |
| Declaration | Omit `kind`, or `"kind": "2d"` | `"kind": "3d"` + `modelPath` |
| State mapping | Sheet rows (alpha auto-splits rows) | AnimationClip name |
| Rendering | Canvas 2D, swap rows by state | WebGL (three.js), cross-fade by state |

### 2D

```json
{
  "id": "ryan",
  "displayName": "Ryan",
  "spritesheetPath": "ryan.png"
}
```

**No grid assumption**: slicing auto-detects rows and columns on the rendering side based on alpha gaps, so both a tidy 8×9 sheet and
an "AI-generated, unevenly spaced" sheet can be sliced. A single image (only one pet) = 1 frame, drawn whole + procedural motion.
When `spritesheetPath` is omitted, the only image file in the directory is used (if there are several, it is discarded as ambiguous — no guessing).

### 3D

```json
{
  "id": "fox3d",
  "displayName": "Fox 3D",
  "kind": "3d",
  "modelPath": "fox.glb",
  "clips": { "working": "Run", "waiting": "Survey", "done": "Jump", "idle": "Walk" }
}
```

- `kind` may be omitted: providing `modelPath`, or having only one `.glb`/`.gltf` in the directory, both infer 3D.
- `clips` may be omitted: clip names are then guessed by keyword (run/walk → working, wait/survey/idle → waiting,
  jump/celebrate → done), degrading further to taking them in order.
- **Framing auto-normalization**: the model is scaled to a uniform size by its bounding box and centered, and the camera is a 3/4 view,
  so models of any scale (a few-centimeter trinket to a several-meter character) fill the pet window without per-pack parameter tuning.
- Animations `crossFadeFrom` on the **same** AnimationMixer (0.25s); state switches are interruptible and do not restart the animation.

### VRM

VRM is also a glTF container (`.vrm`, or a `.glb` with the `VRMC_vrm` / `VRM` extension) and takes the 3D path,
gaining three extra things:

- **State → expression**: `working → relaxed`, `waiting → surprised`, `done → happy`, `idle → neutral`.
  Expressions the model does not have are skipped automatically, without error.
- **Look at mouse**: the cursor position (polled and pushed from the Rust side) is normalized to NDC and drives the VRM's `lookAt`,
  so the eyes/head follow you.
- **Blinking + procedural motion when there is no animation**: blink at random intervals; when the model has no AnimationClip,
  apply a different bob/sway rhythm by state (slow when idle, fast when working, a little hop on done).

three-vrm is likewise lazy-loaded: non-VRM models do not pay this size cost.

## Sidecar Assets and Compressed Formats

`.glb` is a single file and is used directly. `.gltf` may reference external `.bin` / textures, but the WebView cannot read
files under `~/.opencapx/pets/<id>/`, so Core provides `read_pet_asset(id, rel)` (with directory traversal validation),
and the frontend inlines each relative URI into a data URI before handing it to `GLTFLoader.parse()`.

Decoders already wired in (bundled with the app in `public/draco/`, `public/basis/`, **usable offline**):

- **Draco** (`KHR_draco_mesh_compression`)
- **KTX2 / Basis** (compressed textures; the local GPU supports ASTC/S3TC)

Both are wasm loaded on demand: downloaded only when a model actually uses them, and `dispose()`d right after use.

## Import Methods

| Entry point | Accepts |
|---|---|
| Import pet pack | A directory (containing `pet.json`; if there is no manifest but there is a single model/image, a manifest is generated automatically) |
| Import image | A single `.png` / `.webp` |
| Import 3D model | A single `.glb` / `.gltf` |
| URL import | A direct image link (downloaded on the Rust side, bypassing CORS and able to validate content-type) |

Model file limit 128 MB; manifest inline data URL limits: sheet 4 MB, model 12 MB.

## Performance Design (Persistent Window)

The pet window is always-on-top and long-lived, so the 3D path must be power-efficient:

- **Lazy loading**: `three` is a dynamic `import()`. In a production build the pet window entry is only ~18 kB (gzip 6.6 kB),
  while `three` (gzip ~192 kB), `pet3d`, `GLTFLoader`, and the decoders are each a separate chunk — users who only use 2D pay zero extra size.
- **Frame rate capped at 30fps** (2D is 8/4/3fps), and `setPixelRatio(1)` does not scale by DPR.
- **Stop rendering when not visible**: the rAF loop stops when the pet is hidden or the document is not visible.
- A load failure (no WebGL / model decode failure) quietly falls back to the 2D character and does not leave the pet blank.

## Related Implementation

| Location | Responsibility |
|---|---|
| `src-tauri/src/core/petpack.rs` | Manifest parsing, installation, asset reading (including directory traversal validation) |
| `src/petpack.ts` | Frontend pack cache, sheet slicing, `.gltf` sidecar-asset inlining |
| `src/pet3d.ts` | three lazy loading, model loading, clip mapping, render loop, offscreen snapshot |
| `src/overlay.ts` | 2D/3D branch selection, drag hit area, visibility linkage |
| `src/settings.ts` | Cards/thumbnails (3D renders one frame in offscreen WebGL), the `3D` badge, import entry points |
