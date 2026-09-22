import { invoke } from "@tauri-apps/api/core";

/** One cell in the sheet (pixel coordinates, relative to the original image). */
export interface PackFrame {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface PackMeta {
  id: string;
  displayName: string;
  description: string;
  /** "2d" (sheet) or "3d" (glTF model). */
  kind: string;
  sheetPath: string;
  /** 3D model path (empty for 2D packs). */
  modelPath: string;
  /** 3D: state → clip name. */
  clips: Record<string, string>;
}

export interface SlicedPack {
  meta: PackMeta;
  image: HTMLImageElement;
  /** Outer = sheet rows (one animation each), inner = frames within that row. */
  rows: PackFrame[][];
}

/** Alpha below this is treated as transparent (matches the reference implementation, tolerates compression noise). */
const ALPHA_THRESHOLD = 16;

const cache = new Map<string, SlicedPack>();

export function listPacks(): Promise<PackMeta[]> {
  return invoke<PackMeta[]>("list_pet_packs");
}

export function importPack(path: string): Promise<string> {
  return invoke<string>("import_pet_pack", { path });
}

export function deletePack(id: string): Promise<void> {
  return invoke("delete_pet_pack", { id });
}

/**
 * Slice the sheet into "rows → frames" by alpha gaps.
 *
 * Deliberately **does not depend on grid metadata**: first find contiguous ranges where "the whole row has any opaque pixel" as rows,
 * then find contiguous ranges by column within each row. A regular 8×9 sheet slices, and so do
 * AI-generated sheets with uneven spacing / varying frame counts.
 */
export function sliceSheet(
  img: HTMLImageElement,
  alphaThreshold = ALPHA_THRESHOLD,
): PackFrame[][] {
  const w = img.naturalWidth;
  const h = img.naturalHeight;
  if (!w || !h) return [];
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) return [];
  ctx.drawImage(img, 0, 0);
  let data: Uint8ClampedArray;
  try {
    ({ data } = ctx.getImageData(0, 0, w, h));
  } catch {
    return []; // cross-origin sheets cannot read pixels
  }
  const opaque = (x: number, y: number): boolean => data[(y * w + x) * 4 + 3] > alphaThreshold;

  // 1) Row ranges
  const rowSegs: Array<[number, number]> = [];
  let rowStart = -1;
  for (let y = 0; y < h; y++) {
    let any = false;
    for (let x = 0; x < w; x++) {
      if (opaque(x, y)) {
        any = true;
        break;
      }
    }
    if (any && rowStart < 0) rowStart = y;
    else if (!any && rowStart >= 0) {
      rowSegs.push([rowStart, y]);
      rowStart = -1;
    }
  }
  if (rowStart >= 0) rowSegs.push([rowStart, h]);

  // 2) Split columns within rows
  const rows: PackFrame[][] = [];
  for (const [y0, y1] of rowSegs) {
    const frames: PackFrame[] = [];
    let colStart = -1;
    for (let x = 0; x < w; x++) {
      let any = false;
      for (let y = y0; y < y1; y++) {
        if (opaque(x, y)) {
          any = true;
          break;
        }
      }
      if (any && colStart < 0) colStart = x;
      else if (!any && colStart >= 0) {
        frames.push({ x: colStart, y: y0, w: x - colStart, h: y1 - y0 });
        colStart = -1;
      }
    }
    if (colStart >= 0) frames.push({ x: colStart, y: y0, w: w - colStart, h: y1 - y0 });
    if (frames.length > 0) rows.push(frames);
  }
  return rows;
}

/** Decode an image (URL / data URL). Rejects on failure; the caller handles the fallback. */
export function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("image decode failed"));
    img.src = src;
  });
}

/** Load and slice a pet pack (cached). Returns null on failure; the caller falls back to the built-in figure. */
export async function loadPack(id: string): Promise<SlicedPack | null> {
  if (!id) return null;
  const hit = cache.get(id);
  if (hit) return hit;
  try {
    const packs = await listPacks();
    const meta = packs.find((p) => p.id === id);
    // 3D packs have no sheet: hand off to the Pet3D branch
    if (!meta || meta.kind === "3d") return null;
    const dataUrl = await invoke<string>("read_pet_sheet", { id });
    const image = await loadImage(dataUrl);
    const rows = sliceSheet(image);
    if (rows.length === 0) return null;
    const pack: SlicedPack = { meta, image, rows };
    cache.set(id, pack);
    return pack;
  } catch {
    return null;
  }
}

export function forgetPack(id: string): void {
  cache.delete(id);
}

/** 3D pack: model data + manifest (handed to Pet3D). Returns null on failure. */
export interface ModelPack {
  meta: PackMeta;
  /** `.glb` is binary; `.gltf` has external .bin/textures inlined into self-contained JSON text. */
  data: ArrayBuffer | string;
}

export async function loadModelPack(id: string): Promise<ModelPack | null> {
  if (!id) return null;
  const hit = modelCache.get(id);
  if (hit) return hit;
  try {
    const packs = await listPacks();
    const meta = packs.find((p) => p.id === id);
    if (!meta || meta.kind !== "3d") return null;
    // The Rust side returns raw bytes via ipc::Response (not through JSON)
    const bytes = await invoke<ArrayBuffer>("read_pet_model", { id });
    const data: ArrayBuffer | string = meta.modelPath.toLowerCase().endsWith(".gltf")
      ? await inlineGltfAssets(id, new TextDecoder().decode(new Uint8Array(bytes)))
      : bytes;
    const out = { meta, data };
    modelCache.set(id, out);
    return out;
  } catch {
    return null;
  }
}

/**
 * `.gltf` references external resources (`.bin`, textures) by URI, but `GLTFLoader.parse()` cannot reach
 * those paths — our packs live in `~/.opencapx/pets/<id>/`, and the WebView cannot read the filesystem.
 * So first read every relative URI and inline it as a data URI, assembling a self-contained model before parsing.
 */
async function inlineGltfAssets(id: string, json: string): Promise<string> {
  const doc = JSON.parse(json) as {
    buffers?: { uri?: string }[];
    images?: { uri?: string }[];
  };
  const MIME: Record<string, string> = {
    bin: "application/octet-stream",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    webp: "image/webp",
    ktx2: "image/ktx2",
    png: "image/png",
  };
  const inline = async (item: { uri?: string }): Promise<void> => {
    const uri = item.uri;
    if (typeof uri !== "string" || !uri || /^(data:|https?:|blob:)/i.test(uri)) return;
    const rel = decodeURIComponent(uri);
    const buf = await invoke<ArrayBuffer>("read_pet_asset", { id, rel });
    const ext = rel.split(".").pop()?.toLowerCase() ?? "";
    item.uri = `data:${MIME[ext] ?? "application/octet-stream"};base64,${toBase64(
      new Uint8Array(buf),
    )}`;
  };
  for (const item of doc.buffers ?? []) await inline(item);
  for (const item of doc.images ?? []) await inline(item);
  return JSON.stringify(doc);
}

/** Chunked base64: passing hundreds of thousands of arguments to a single `String.fromCharCode(...)` blows the stack. */
function toBase64(bytes: Uint8Array): string {
  const CHUNK = 0x8000;
  let out = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    out += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(out);
}

const modelCache = new Map<string, ModelPack>();

export function forgetModelPack(id: string): void {
  modelCache.delete(id);
}

/** Get a mood's row (wraps when out of range). */
export function rowForMood(pack: SlicedPack, row: number): PackFrame[] {
  return pack.rows[row % pack.rows.length] ?? pack.rows[0];
}
