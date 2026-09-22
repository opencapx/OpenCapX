/**
 * 3D pet rendering (glTF / GLB).
 *
 * Design notes:
 * - **Lazy-load three**: `import("three")` only happens when a 3D pack is actually selected; 2D users do not pay that bundle size.
 * - **One clip per state**: one AnimationClip maps to one mood, isomorphic to 2D's "one row = one state";
 *   switch states with `crossFadeTo` (interruptible), without restarting the animation.
 * - **Power saving**: frame-rate cap + stop rendering when the window/document is not visible — a persistent always-on-top window
 *   running the GPU continuously really does drain the battery.
 * - **Normalized framing**: model sizes vary wildly, so first scale to a uniform size by bounding box, then place the camera, lest
 *   some models look like tiny gray dots while others blow out the window.
 *
 * The classic WebGL pitfall in a transparent window: `alpha: true` + clearing to fully transparent + `premultipliedAlpha`
 * and how it interacts with WebKit compositing. Here we use three's default (premultiplied) and only need to adjust
 * when dark edges are detected — see the comment in `create()`.
 */

type Three = typeof import("three");
type VrmModule = typeof import("@pixiv/three-vrm");
type Vrm = import("@pixiv/three-vrm").VRM;

export type Mood3D = "working" | "waiting" | "done" | "idle";

/** VRM: state → expression preset (VRM 1.0 preset name). Missing ones are skipped automatically. */
const VRM_MOOD_EXPRESSION: Record<Mood3D, string> = {
  working: "relaxed",
  waiting: "surprised",
  done: "happy",
  idle: "neutral",
};

let vrmMod: Promise<VrmModule> | null = null;
/** Lazy-load three-vrm: loaded only when a VRM model is actually selected (same as three, kept out of the main bundle). */
function loadVrm(): Promise<VrmModule> {
  if (!vrmMod) vrmMod = import("@pixiv/three-vrm");
  return vrmMod;
}

/** Detect whether it is VRM: by extension, or a glTF carrying a VRM extension (0.x's `VRM` / 1.0's `VRMC_vrm`). */
export function isVrmModel(data: ArrayBuffer | string): boolean {
  if (typeof data === "string") return data.includes("VRMC_vrm") || data.includes('"VRM"');
  // GLB's JSON chunk comes first (BIN after), so scanning the first 256KB is enough; no need to walk the whole model
  const head = new Uint8Array(data.slice(0, Math.min(data.byteLength, 256 * 1024)));
  const needle = new TextEncoder().encode("VRMC_vrm");
  const other = new TextEncoder().encode('"VRM"');
  const has = (n: Uint8Array): boolean => {
    outer: for (let i = 0; i + n.length <= head.length; i += 1) {
      for (let j = 0; j < n.length; j += 1) if (head[i + j] !== n[j]) continue outer;
      return true;
    }
    return false;
  };
  return has(needle) || has(other);
}

/** Frame-rate cap: the pet is background decoration and does not need 60fps burning power. */
const MAX_FPS = 30;
/** Cross-fade duration for state switches (seconds). */
const FADE = 0.25;
/** Target size of the model's longest edge in the scene after normalization (view height ~2.5, leaving breathing room). */
const NORM_SIZE = 1.9;

let threeMod: Promise<Three> | null = null;
/** Lazy-load three (and GLTFLoader). */
function loadThree(): Promise<Three> {
  if (!threeMod) threeMod = import("three");
  return threeMod;
}

type GltfBundle = {
  GLTFLoader: typeof import("three/examples/jsm/loaders/GLTFLoader.js").GLTFLoader;
  DRACOLoader: typeof import("three/examples/jsm/loaders/DRACOLoader.js").DRACOLoader;
  KTX2Loader: typeof import("three/examples/jsm/loaders/KTX2Loader.js").KTX2Loader;
};

let gltfMod: Promise<GltfBundle> | null = null;
/**
 * Lazy-load GLTFLoader + two decoders. The decoders (Draco / KTX2) are wasm
 * and are downloaded only when a compressed model is actually encountered, so they must be dynamic imports too — a static import
 * would bundle them into the main package, making 2D users pay the size for nothing.
 */
function loadGltf(): Promise<GltfBundle> {
  if (!gltfMod) {
    gltfMod = Promise.all([
      import("three/examples/jsm/loaders/GLTFLoader.js"),
      import("three/examples/jsm/loaders/DRACOLoader.js"),
      import("three/examples/jsm/loaders/KTX2Loader.js"),
    ]).then(([g, d, k]) => ({
      GLTFLoader: g.GLTFLoader,
      DRACOLoader: d.DRACOLoader,
      KTX2Loader: k.KTX2Loader,
    }));
  }
  return gltfMod;
}

/** Pick a clip by mood: explicit mapping → name keywords → order → first. */
export function pickClipName(
  clips: Record<string, string>,
  available: string[],
  mood: Mood3D,
): string {
  const explicit = clips?.[mood]?.trim();
  if (explicit && available.some((n) => n.toLowerCase() === explicit.toLowerCase())) {
    return available.find((n) => n.toLowerCase() === explicit.toLowerCase())!;
  }
  const lower = available.map((n) => n.toLowerCase());
  const find = (keys: string[]): string | undefined => {
    const i = lower.findIndex((n) => keys.some((k) => n.includes(k)));
    return i >= 0 ? available[i] : undefined;
  };
  const byKeyword: Record<Mood3D, string[]> = {
    working: ["run", "walk", "move", "work", "action", "sprint", "gallop"],
    waiting: ["wait", "survey", "idle", "look", "think", "listen"],
    done: ["jump", "celebrate", "dance", "happy", "success", "win"],
    idle: ["idle", "walk", "breath", "sit", "rest"],
  };
  const hit = find(byKeyword[mood]);
  if (hit) return hit;
  const order: Mood3D[] = ["working", "waiting", "done", "idle"];
  const idx = order.indexOf(mood);
  if (available.length > 0 && idx >= 0) {
    return available[Math.min(idx, available.length - 1)];
  }
  return available[0] ?? "";
}

export class Pet3D {
  private renderer: import("three").WebGLRenderer;
  private scene: import("three").Scene;
  private camera: import("three").PerspectiveCamera;
  private mixer: import("three").AnimationMixer | null = null;
  private actions = new Map<string, import("three").AnimationAction>();
  private current: string = "";
  private root: import("three").Object3D | null = null;
  private raf = 0;
  private last = 0;
  private running = false;
  private mood: Mood3D = "idle";
  private canvas: HTMLCanvasElement;

  private constructor(
    canvas: HTMLCanvasElement,
    renderer: import("three").WebGLRenderer,
    scene: import("three").Scene,
    camera: import("three").PerspectiveCamera,
  ) {
    this.canvas = canvas;
    this.renderer = renderer;
    this.scene = scene;
    this.camera = camera;
  }

  /** Create a renderer bound to the canvas (the caller handles the canvas's show/hide). */
  static async create(canvas: HTMLCanvasElement): Promise<Pet3D> {
    const THREE = await loadThree();
    const renderer = new THREE.WebGLRenderer({
      canvas,
      alpha: true,
      antialias: true,
      // The pet window is transparent: do not enable preserveDrawingBuffer for convenience (it makes compositing more expensive);
      // snapshots use "render first, then read" to get pixels.
      preserveDrawingBuffer: false,
    });
    renderer.setClearColor(0x000000, 0);
    renderer.setPixelRatio(1); // persistent window: do not scale by DPR, saving over half the pixels
    renderer.outputColorSpace = THREE.SRGBColorSpace;

    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(35, 1, 0.1, 100);
    // 3/4 view: quadrupeds and humanoids both reveal more of a side, looking better and larger than facing the camera straight on
    camera.position.set(1.15, 0.55, 3.4);
    camera.lookAt(0, 0, 0);

    // A simplified three-point setup: hemisphere base + key light + fill light, enough to keep any model from going pitch black
    scene.add(new THREE.HemisphereLight(0xffffff, 0x444455, 1.6));
    const key = new THREE.DirectionalLight(0xffffff, 2.2);
    key.position.set(2, 3, 4);
    scene.add(key);
    const fill = new THREE.DirectionalLight(0xffffff, 0.8);
    fill.position.set(-3, -1, -2);
    scene.add(fill);

    return new Pet3D(canvas, renderer, scene, camera);
  }

  /** Load a .glb (binary) or self-contained .gltf (JSON text) and normalize framing. */
  async load(data: ArrayBuffer | string, clips: Record<string, string>): Promise<void> {
    const THREE = await loadThree();
    const { GLTFLoader, DRACOLoader, KTX2Loader } = await loadGltf();
    const loader = new GLTFLoader();
    // Decoder paths point to public/ (bundled with the app, usable offline)
    const draco = new DRACOLoader();
    draco.setDecoderPath("/draco/");
    loader.setDRACOLoader(draco);
    const ktx2 = new KTX2Loader().setTranscoderPath("/basis/");
    ktx2.detectSupport(this.renderer);
    loader.setKTX2Loader(ktx2);
    // The VRM plugin must be registered before parse (it takes over MToon materials, expressions, humanoid, spring bones)
    if (isVrmModel(data)) {
      const { VRMLoaderPlugin } = await loadVrm();
      loader.register((parser) => new VRMLoaderPlugin(parser));
    }
    const gltf = await new Promise<import("three/examples/jsm/loaders/GLTFLoader.js").GLTF>(
      (resolve, reject) => {
        loader.parse(data, "", resolve, reject);
      },
    ).finally(() => {
      // Release decoders once used (an unreleased wasm worker keeps holding memory)
      draco.dispose();
      ktx2.dispose();
    });
    this.disposeRoot();
    this.root = gltf.scene;
    this.scene.add(this.root);

    // Normalization: scale the model to a uniform size by bounding box and center it; the camera needs no change
    const box = new THREE.Box3().setFromObject(this.root);
    const size = box.getSize(new THREE.Vector3());
    const center = box.getCenter(new THREE.Vector3());
    const maxDim = Math.max(size.x, size.y, size.z) || 1;
    const k = NORM_SIZE / maxDim;
    this.root.scale.setScalar(k);
    this.root.position.set(-center.x * k, -center.y * k, -center.z * k);

    // VRM: hand expressions/lookAt/spring bones to three-vrm. Without animation clips, rely on procedural motion + blinking.
    const vrm = gltf.userData?.vrm as Vrm | undefined;
    if (vrm) {
      const { VRMUtils } = await loadVrm();
      VRMUtils.combineSkeletons(this.root); // merge skeleton nodes to cut per-draw-call overhead
      VRMUtils.rotateVRM0(vrm); // VRM 0.x faces -Z, rotate to face the camera
      this.vrm = vrm;
      const target = new THREE.Object3D();
      this.scene.add(target);
      this.lookTarget = target;
      if (vrm.lookAt) vrm.lookAt.target = target;
      this.applyVrmExpression();
    }

    this.mixer = new THREE.AnimationMixer(this.root);
    this.actions.clear();
    this.current = "";
    for (const clip of gltf.animations) {
      this.actions.set(clip.name, this.mixer.clipAction(clip));
    }
    this.clipNames = gltf.animations.map((c) => c.name);
    this.clipMap = clips ?? {};
    this.baseY = this.root.position.y;
    this.baseRotY = this.root.rotation.y;
    this.elapsed = 0;
    this.applyMood(true);
  }

  private clipNames: string[] = [];
  private clipMap: Record<string, string> = {};
  // VRM: expressions / look-at cursor / blinking. All null for non-VRM models, affecting no path.
  private vrm: Vrm | null = null;
  private lookTarget: import("three").Object3D | null = null;
  private blinkAt = 0;
  private blinkUntil = 0;
  private cursor: { x: number; y: number } | null = null;
  private elapsed = 0;
  /** Base pose after normalization: procedural motion "adds on top of the base" and must not push the model ever further away. */
  private baseY = 0;
  private baseRotY = 0;

  /** Cursor position (NDC, -1..1): VRM looks at it. */
  setCursor(nx: number, ny: number): void {
    this.cursor = { x: Math.max(-1, Math.min(1, nx)), y: Math.max(-1, Math.min(1, ny)) };
  }

  /** Procedural liveliness when there is no skeletal animation: bob + sway, changing rhythm by state. */
  private proceduralMotion(dt: number): void {
    if (!this.root || this.clipNames.length > 0) return;
    this.elapsed += dt;
    const t = this.elapsed;
    const speed: Record<Mood3D, number> = { working: 6, waiting: 2.2, done: 9, idle: 1.6 };
    const amp: Record<Mood3D, number> = { working: 0.035, waiting: 0.02, done: 0.06, idle: 0.022 };
    const s = speed[this.mood];
    const a = amp[this.mood];
    const hop = this.mood === "done" ? Math.abs(Math.sin(t * s)) * a * 1.6 : 0;
    this.root.position.y = this.baseY + Math.sin(t * s) * a + hop;
    this.root.rotation.y = this.baseRotY + Math.sin(t * (s / 2.4)) * 0.09;
    // When idle, tilt slightly down/up as if looking around
    this.root.rotation.x = this.mood === "waiting" ? Math.sin(t * 0.9) * 0.05 : 0;
  }

  /** Blink: a flick at random intervals, the cheapest "alive" signal. */
  private tickBlink(now: number): void {
    if (!this.vrm) return;
    const em = this.vrm.expressionManager;
    if (!em) return;
    if (this.blinkAt === 0) this.blinkAt = now + 1200 + Math.random() * 2500;
    if (now >= this.blinkAt && this.blinkUntil === 0) this.blinkUntil = now + 110;
    const closed = this.blinkUntil > now;
    if (this.blinkUntil !== 0 && !closed) {
      this.blinkUntil = 0;
      this.blinkAt = now + 1500 + Math.random() * 3000;
    }
    em.setValue("blink", closed ? 1 : 0);
  }

  /** Look at cursor: place a target point in front of the camera; VRM's lookAt drives the eyes/head. */
  private aimLookAt(): void {
    if (!this.vrm || !this.lookTarget) return;
    const c = this.cursor ?? { x: 0, y: 0 };
    const dist = 3;
    const halfH = Math.tan(((this.camera.fov / 2) * Math.PI) / 180) * dist;
    const halfW = halfH * this.camera.aspect;
    // The camera is fixed at (x,y,z) looking at the origin; converting directly from camera coordinates is simplest
    const p = this.camera.position.clone();
    const dir = p.clone().negate().normalize();
    const right = dir.clone().cross(this.camera.up).normalize();
    const up = right.clone().cross(dir).normalize();
    this.lookTarget.position
      .copy(p)
      .addScaledVector(dir, dist)
      .addScaledVector(right, c.x * halfW)
      .addScaledVector(up, c.y * halfH);
  }

  /** Apply the VRM expression for the current state. */
  private applyVrmExpression(): void {
    const em = this.vrm?.expressionManager;
    if (!em) return;
    for (const name of Object.values(VRM_MOOD_EXPRESSION)) {
      if (!em.getExpression(name)) continue;
      em.setValue(name, 0);
    }
    const want = VRM_MOOD_EXPRESSION[this.mood];
    if (want && em.getExpression(want)) em.setValue(want, 1);
  }

  private applyMood(immediate = false): void {
    if (!this.mixer || this.clipNames.length === 0) return;
    const name = pickClipName(this.clipMap, this.clipNames, this.mood);
    if (!name || name === this.current) return;
    const next = this.actions.get(name);
    if (!next) return;
    const prev = this.current ? this.actions.get(this.current) : undefined;
    next.reset();
    next.enabled = true;
    next.setEffectiveWeight(1);
    next.play();
    if (prev && prev !== next && !immediate) {
      next.crossFadeFrom(prev, FADE, true); // interruptible: switching state again restarts the cross-fade
    } else if (prev && prev !== next) {
      prev.stop();
    }
    this.current = name;
  }

  setMood(mood: Mood3D): void {
    if (mood === this.mood) return;
    this.mood = mood;
    this.applyMood();
    this.applyVrmExpression();
  }

  /** Canvas logical size (the pet canvas is square). */
  setSize(px: number): void {
    const size = Math.max(16, Math.round(px));
    this.renderer.setSize(size, size, false);
    this.camera.aspect = 1;
    this.camera.updateProjectionMatrix();
    this.render(false);
  }

  /** Render one frame (used for snapshots and the first frame). */
  render(advance = true): void {
    if (advance && this.mixer) this.mixer.update(0);
    this.renderer.render(this.scene, this.camera);
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.last = performance.now();
    const tick = (t: number): void => {
      if (!this.running) return;
      this.raf = requestAnimationFrame(tick);
      const minGap = 1000 / MAX_FPS;
      const dt = t - this.last;
      if (dt < minGap) return;
      this.last = t - (dt % minGap);
      const sec = Math.min(dt / 1000, 0.1);
      if (this.mixer) this.mixer.update(sec);
      this.proceduralMotion(sec);
      if (this.vrm) {
        this.aimLookAt();
        this.tickBlink(t);
        this.vrm.update(sec); // spring bones / lookAt / expressions all take effect here
      }
      this.renderer.render(this.scene, this.camera);
    };
    this.raf = requestAnimationFrame(tick);
  }

  /** Pause rendering (called when the window is invisible / the document is hidden) — key to a power-frugal persistent pet. */
  stop(): void {
    this.running = false;
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
  }

  get isRunning(): boolean {
    return this.running;
  }

  /** Name of the currently playing clip (for diagnostics). */
  get currentClip(): string {
    return this.current;
  }

  get clipCount(): number {
    return this.clipNames.length;
  }

  /** Whether it is VRM (has expressions/lookAt/spring bones). For diagnostics and settings-page display. */
  get isVrm(): boolean {
    return this.vrm !== null;
  }

  /** Expression name for the current state (VRM). */
  get currentExpression(): string {
    return this.vrm ? VRM_MOOD_EXPRESSION[this.mood] : "";
  }

  /** Render one frame and export a data URL (settings-page thumbnail). */
  snapshot(size: number): string {
    const prevW = this.canvas.width;
    const prevH = this.canvas.height;
    this.setSize(size);
    this.renderer.render(this.scene, this.camera);
    const url = this.canvas.toDataURL();
    this.setSize(prevW || prevH || size);
    return url;
  }

  private disposeRoot(): void {
    if (!this.root) return;
    if (this.lookTarget) {
      this.scene.remove(this.lookTarget);
      this.lookTarget = null;
    }
    this.vrm = null;
    this.root.traverse((o) => {
      const m = o as import("three").Mesh;
      m.geometry?.dispose?.();
      const mat = m.material as import("three").Material | import("three").Material[] | undefined;
      if (Array.isArray(mat)) mat.forEach((x) => x.dispose());
      else mat?.dispose?.();
    });
    this.scene.remove(this.root);
    this.root = null;
    this.mixer = null;
    this.actions.clear();
    this.clipNames = [];
  }

  dispose(): void {
    this.stop();
    this.disposeRoot();
    this.renderer.dispose();
  }
}

/** Offscreen snapshotter for the settings page: reuses one renderer to avoid repeatedly creating WebGL contexts. */
let thumbPet: Pet3D | null = null;
let thumbBusy: Promise<string> | null = null;

/** Model bytes → snapshot at the given size (data URL). Returns an empty string on failure. */
export async function renderModelSnapshot(
  data: ArrayBuffer | string,
  clips: Record<string, string>,
  size: number,
  mood: Mood3D = "idle",
): Promise<string> {
  // Serialize: one renderer serves only one model at a time
  const run = async (): Promise<string> => {
    if (!thumbPet) {
      const c = document.createElement("canvas");
      c.width = 256;
      c.height = 256;
      thumbPet = await Pet3D.create(c);
    }
    await thumbPet.load(data, clips);
    thumbPet.setMood(mood);
    // Advance time a little so snapshots do not all freeze on animation frame 0
    thumbPet.render(true);
    return thumbPet.snapshot(size);
  };
  const prev = thumbBusy ?? Promise.resolve("");
  const next = prev.then(run, run);
  thumbBusy = next.catch(() => "");
  return thumbBusy;
}
