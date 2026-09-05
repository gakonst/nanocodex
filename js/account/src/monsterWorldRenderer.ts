import type { Direction, WorldSceneId } from "./monsterWorldProtocol.ts";
import {
  WORLD_PIXEL_HEIGHT,
  WORLD_PIXEL_WIDTH,
  WORLD_POIS,
  WORLD_PORTALS,
  WORLD_SCENES,
  WORLD_TILE_SIZE,
  WORLD_VIEW_COLUMNS,
  WORLD_VIEW_ROWS,
  worldToViewport,
  type WorldCamera,
} from "./monsterWorldMap.ts";
import {
  actorRenderPoint,
  actorsInPaintOrder,
  actorWorldPosition,
  worldCameraForState,
  type WorldActor,
  type WorldState,
} from "./monsterWorldSimulation.ts";

const TILE = WORLD_TILE_SIZE;
const ASSET_ROOT = "/world/my-pixel-world";
const WORLD_ASSET_VERSION =
  "0c334ab5204e71d019abe47e53bffd174cbb7589b8882999c70a0fe37c0e169b";
const TOWN_ORCHARD_TREES = [
  [4, 27],
  [8, 27],
  [12, 27],
  [16, 27],
  [4, 32],
  [8, 32],
  [12, 32],
  [16, 32],
  [4, 37],
  [8, 37],
  [12, 37],
  [16, 37],
] as const;
const TOWN_SCENERY_TREES = [
  [34, 17],
  [35, 17],
  [58, 22],
  [59, 22],
] as const;

export type WorldAssets = Readonly<{
  tileset?: HTMLImageElement;
  humans: Partial<Record<number, HTMLImageElement>>;
  monsters: Partial<Record<number, HTMLImageElement>>;
}>;

let assetRequest: Promise<WorldAssets> | undefined;

export function loadWorldAssets(): Promise<WorldAssets> {
  assetRequest ??= Promise.all([
    loadImage(worldAssetSource("tileset/tileset.png")),
    Promise.all(
      Array.from({ length: 10 }, (_, index) =>
        loadImage(worldAssetSource(`character-overworld/ow${index + 1}.png`)),
      ),
    ),
    Promise.all(
      Array.from({ length: 16 }, (_, index) =>
        loadImage(worldAssetSource(`sprites/sprite${index + 1}_idle.png`)),
      ),
    ),
  ]).then(([tileset, humans, monsters]) =>
    Object.freeze({
      ...(tileset ? { tileset } : {}),
      humans: Object.freeze(
        Object.fromEntries(humans.map((image, index) => [index + 1, image])),
      ),
      monsters: Object.freeze(
        Object.fromEntries(monsters.map((image, index) => [index + 1, image])),
      ),
    }),
  );
  return assetRequest;
}

export function worldAssetSource(path: string): string {
  return `${ASSET_ROOT}/${path}?v=${WORLD_ASSET_VERSION}`;
}

export function drawMonsterWorld(
  context: CanvasRenderingContext2D,
  state: WorldState,
  assets: WorldAssets | undefined,
  options: Readonly<{ reducedMotion: boolean; pixelRatio: number }>,
): void {
  const pixelRatio = normalizedPixelRatio(options.pixelRatio);
  ensureRenderViewport(context.canvas, pixelRatio);
  const camera = worldCameraForState(state);

  context.setTransform(
    context.canvas.width / WORLD_PIXEL_WIDTH,
    0,
    0,
    context.canvas.height / WORLD_PIXEL_HEIGHT,
    0,
    0,
  );
  context.save();
  context.imageSmoothingEnabled = false;
  context.clearRect(0, 0, WORLD_PIXEL_WIDTH, WORLD_PIXEL_HEIGHT);
  drawScene(context, state, camera, assets?.tileset, options.reducedMotion);
  drawSpeechWave(context, state, camera, options.reducedMotion);

  const actors = actorsInPaintOrder(state).filter((actor) =>
    isActorVisible(actor, camera),
  );
  for (const actor of actors) {
    drawActor(context, state, camera, actor, assets, options.reducedMotion);
  }

  drawSceneForeground(context, camera, assets?.tileset);
  drawAtmosphere(context, state, camera.scene, options.reducedMotion);
  for (const actor of actors) drawActorOverlay(context, state, camera, actor);
  drawScenePlaque(context, camera.scene);
  context.restore();
}

function normalizedPixelRatio(value: number): number {
  if (!Number.isFinite(value)) return 1;
  return Math.max(1, Math.min(3, value));
}

function ensureRenderViewport(
  canvas: HTMLCanvasElement,
  pixelRatio: number,
): void {
  const cssWidth = canvas.clientWidth > 0 ? canvas.clientWidth : WORLD_PIXEL_WIDTH;
  const width = Math.max(WORLD_PIXEL_WIDTH, Math.round(cssWidth * pixelRatio));
  const height = Math.max(
    WORLD_PIXEL_HEIGHT,
    Math.round(width * WORLD_PIXEL_HEIGHT / WORLD_PIXEL_WIDTH),
  );
  if (
    canvas.width !== width ||
    canvas.height !== height
  ) {
    canvas.width = width;
    canvas.height = height;
  }
}

function drawScene(
  context: CanvasRenderingContext2D,
  state: WorldState,
  camera: WorldCamera,
  tileset: HTMLImageElement | undefined,
  reducedMotion: boolean,
): void {
  withWorldCamera(context, camera, () => {
    if (camera.scene === "town") {
      drawTown(context, state, tileset, reducedMotion);
    } else if (camera.scene === "guild_hall") {
      drawGuildHall(context, state, reducedMotion);
    } else {
      drawTrailShop(context, state, reducedMotion);
    }
    drawScenePortals(context, state, camera.scene, reducedMotion);
  });
}

function drawSceneForeground(
  context: CanvasRenderingContext2D,
  camera: WorldCamera,
  tileset: HTMLImageElement | undefined,
): void {
  withWorldCamera(context, camera, () => {
    if (camera.scene === "town") {
      drawTownForeground(context, tileset);
    } else if (camera.scene === "guild_hall") {
      drawGuildHallForeground(context);
    } else {
      drawTrailShopForeground(context);
    }
  });
}

function withWorldCamera(
  context: CanvasRenderingContext2D,
  camera: WorldCamera,
  draw: () => void,
): void {
  context.save();
  context.beginPath();
  context.rect(0, 0, WORLD_PIXEL_WIDTH, WORLD_PIXEL_HEIGHT);
  context.clip();
  context.translate(-camera.x * TILE, -camera.y * TILE);
  draw();
  context.restore();
}

function drawTown(
  context: CanvasRenderingContext2D,
  state: WorldState,
  tileset: HTMLImageElement | undefined,
  reducedMotion: boolean,
): void {
  drawTownGround(context, state.elapsedMs, reducedMotion);
  drawTownOrchardFloor(context);
  drawTownTrainingMeadow(context);
  drawTownPaths(context);
  drawTownPlaza(context);
  drawTownWater(context, state.elapsedMs, reducedMotion);
  drawTownBuildings(context, tileset);
  drawTownOrchard(context, tileset);
  drawTownScenery(context, tileset);
  drawTownBoundary(context, tileset);
  drawTownProps(context, state);
}

function drawTownForeground(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
): void {
  drawTownDoorwayForeground(context, tileset);
  for (const [x, y] of TOWN_ORCHARD_TREES) {
    drawTreeCanopyAtTile(context, tileset, x, y, true);
  }
  for (const [x, y] of TOWN_SCENERY_TREES) {
    drawTreeCanopyAtTile(context, tileset, x, y, false);
  }
}

function drawTownGround(
  context: CanvasRenderingContext2D,
  elapsedMs: number,
  reducedMotion: boolean,
): void {
  const scene = WORLD_SCENES.town;
  context.fillStyle = "#aab789";
  context.fillRect(0, 0, scene.columns * TILE, scene.rows * TILE);
  const phase = reducedMotion ? 0 : Math.floor(elapsedMs / 900) % 2;
  for (let y = 0; y < scene.rows; y += 1) {
    for (let x = 0; x < scene.columns; x += 1) {
      const hash = (x * 17 + y * 31) % 19;
      if (hash !== 0 && hash !== 7 && hash !== 13) continue;
      context.fillStyle =
        (hash + phase) % 2 === 0 ? "#879b74" : "#c0c99a";
      context.fillRect(x * TILE + 2, y * TILE + 4, 3, 1);
      if (hash === 13) context.fillRect(x * TILE + 5, y * TILE + 2, 1, 2);
    }
  }
}

function drawTownPaths(context: CanvasRenderingContext2D): void {
  drawDirtPath(context, 0, 9, 31, 5);
  drawDirtPath(context, 10, 8, 13, 9);
  drawDirtPath(context, 29, 0, 5, 48);
  drawDirtPath(context, 5, 7, 3, 6);
  drawDirtPath(context, 25, 7, 3, 7);
  drawDirtPath(context, 9, 13, 3, 18);
  drawDirtPath(context, 9, 29, 25, 3);
  drawDirtPath(context, 30, 32, 34, 5);
  drawDirtPath(context, 10, 39, 23, 5);
  drawDirtPath(context, 31, 11, 30, 3);

  context.fillStyle = "#ede5c6";
  context.fillRect(0, 10 * TILE + 3, 31 * TILE, 2);
  context.fillRect(31 * TILE + 3, 0, 2, 48 * TILE);
  context.fillRect(10 * TILE + 3, 13 * TILE, 2, 18 * TILE);
  context.fillRect(31 * TILE, 34 * TILE + 3, 33 * TILE, 2);
}

function drawDirtPath(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
): void {
  context.fillStyle = "#d7cfaa";
  fillTileRect(context, x, y, width, height);
  context.fillStyle = "#c9bd97";
  for (let row = y; row < y + height; row += 2) {
    for (let column = x + ((row + x) % 3); column < x + width; column += 5) {
      context.fillRect(column * TILE + 2, row * TILE + 6, 3, 1);
    }
  }
}

function drawTownPlaza(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#c8c3aa";
  fillTileRect(context, 11, 8, 12, 9);
  context.fillStyle = "#ddd8bd";
  for (let y = 8; y < 17; y += 1) {
    for (let x = 11; x < 23; x += 1) {
      if ((x + y) % 2 === 0) {
        context.fillRect(x * TILE + 1, y * TILE + 1, 6, 6);
      }
    }
  }
  context.strokeStyle = "#9d9e8d";
  context.lineWidth = 1;
  context.strokeRect(12 * TILE, 9 * TILE, 10 * TILE, 7 * TILE);
  drawLabel(context, "GUILD PLAZA", 13 * TILE, 15 * TILE);
}

function drawTownWater(
  context: CanvasRenderingContext2D,
  elapsedMs: number,
  reducedMotion: boolean,
): void {
  context.fillStyle = "#657f89";
  fillTileRect(context, 46, 31, 17, 16);
  context.fillStyle = "#88a5a5";
  context.fillRect(46 * TILE, 31 * TILE, 17 * TILE, 3);
  context.fillRect(46 * TILE, 47 * TILE - 3, 17 * TILE, 3);

  const phase = reducedMotion ? 0 : Math.floor(elapsedMs / 420) % 9;
  context.fillStyle = "#a5bbb0";
  for (let y = 32; y < 47; y += 2) {
    for (let x = 47; x < 63; x += 3) {
      context.fillRect(x * TILE + ((phase + y) % 5), y * TILE + 4, 7, 1);
    }
  }

  context.fillStyle = "#454f4c";
  context.fillRect(46 * TILE, 34 * TILE - 2, 17 * TILE, 12);
  context.fillStyle = "#cfb77e";
  context.fillRect(46 * TILE, 34 * TILE, 17 * TILE, TILE);
  context.fillStyle = "#9d7e58";
  for (let x = 46; x < 63; x += 1) {
    context.fillRect(x * TILE + 7, 34 * TILE, 1, TILE);
  }
  context.fillRect(46 * TILE, 34 * TILE, 17 * TILE, 1);
  context.fillRect(46 * TILE, 35 * TILE - 1, 17 * TILE, 1);
  context.fillStyle = "#e5d29a";
  for (let x = 47; x < 63; x += 4) {
    context.fillRect(x * TILE, 34 * TILE - 3, 2, 3);
    context.fillRect(x * TILE, 35 * TILE, 2, 3);
  }
  drawLabel(context, "WHISPER POND", 47 * TILE, 29 * TILE);
  drawLabel(context, "BELL BRIDGE", 49 * TILE, 36 * TILE + 1);
}

function drawTownTrainingMeadow(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#94a97b";
  fillTileRect(context, 19, 34, 25, 12);
  context.strokeStyle = "#d9d3ae";
  context.lineWidth = 1;
  context.strokeRect(22 * TILE, 36 * TILE, 9 * TILE, 7 * TILE);
  context.strokeRect(24 * TILE, 38 * TILE, 5 * TILE, 3 * TILE);
  context.fillStyle = "#d9d3ae";
  context.fillRect(26 * TILE + 3, 38 * TILE + 3, 2, 2);
  context.fillStyle = "#6f8267";
  for (let x = 20; x < 44; x += 4) {
    context.fillRect(x * TILE + 2, 45 * TILE, 4, 2);
  }
  drawTrainingTarget(context, 37, 38);
  drawTrainingTarget(context, 41, 42);
  drawLabel(context, "TRAINING MEADOW", 21 * TILE, 44 * TILE);
}

function drawTrainingTarget(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
): void {
  const x = tileX * TILE + 4;
  const y = tileY * TILE + 4;
  context.fillStyle = "#5f5547";
  context.fillRect(x - 1, y + 2, 2, 7);
  context.fillStyle = "#e1d4a7";
  context.fillRect(x - 4, y - 4, 8, 8);
  context.fillStyle = "#a76e55";
  context.fillRect(x - 2, y - 2, 4, 4);
  context.fillStyle = "#e8dfbd";
  context.fillRect(x - 1, y - 1, 2, 2);
}

function drawTownBuildings(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
): void {
  context.fillStyle = "rgba(25, 30, 28, .28)";
  context.fillRect(2 * TILE - 2, 2 * TILE + 5, 8 * TILE + 4, 6 * TILE);
  context.fillRect(23 * TILE - 2, 2 * TILE + 5, 7 * TILE + 4, 6 * TILE);

  if (tileset) {
    // Keep the original elemental guild and trail-shop atlas crops.
    context.drawImage(tileset, 480, 0, 112, 112, 2 * TILE, 0, 8 * TILE, 8 * TILE);
    context.drawImage(tileset, 480, 144, 80, 72, 23 * TILE, TILE, 7 * TILE, 50);

    // The distant clinic and cottage use the same loaded atlas.
    context.drawImage(tileset, 384, 144, 80, 112, 3 * TILE, 38 * TILE, 7 * TILE, 9 * TILE);
    context.drawImage(tileset, 560, 144, 112, 72, 52 * TILE, 3 * TILE, 8 * TILE, 8 * TILE);
  } else {
    drawFallbackBuilding(context, 2 * TILE, TILE, 8 * TILE, 7 * TILE, "GUILD");
    drawFallbackBuilding(context, 23 * TILE, 2 * TILE, 7 * TILE, 6 * TILE, "SHOP");
    drawFallbackBuilding(context, 3 * TILE, 39 * TILE, 7 * TILE, 8 * TILE, "CLINIC");
    drawFallbackBuilding(context, 52 * TILE, 4 * TILE, 8 * TILE, 7 * TILE, "COTTAGES");
  }

  drawGuildSign(context);
  drawDungeonApproach(context);
  drawLabel(context, "RESCUE GUILD", 2 * TILE, 8 * TILE + 1);
  drawLabel(context, "TRAIL SHOP", 23 * TILE, 8 * TILE + 1);
  drawLabel(context, "WAYFARER CLINIC", 2 * TILE, 46 * TILE - 1);
  drawLabel(context, "HILLSIDE COTTAGES", 51 * TILE, 11 * TILE);
}

function drawTownDoorwayForeground(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
): void {
  if (tileset) {
    // Redraw only the atlas pixels over each doorway, not the whole building.
    context.drawImage(
      tileset,
      522,
      84,
      42,
      14,
      5 * TILE,
      6 * TILE,
      3 * TILE,
      TILE,
    );
    context.drawImage(
      tileset,
      503,
      202,
      34,
      11,
      25 * TILE,
      6 * TILE,
      3 * TILE,
      TILE,
    );
    return;
  }

  drawFallbackDoorwayForeground(context, 6);
  drawFallbackDoorwayForeground(context, 26);
}

function drawFallbackDoorwayForeground(
  context: CanvasRenderingContext2D,
  doorTileX: number,
): void {
  context.fillStyle = "#3f4b46";
  context.fillRect((doorTileX - 1) * TILE, 6 * TILE, 3 * TILE, TILE);
  context.fillStyle = "#5b6661";
  context.fillRect((doorTileX - 1) * TILE - 2, 6 * TILE, 3 * TILE + 4, 4);
}

function drawGuildSign(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#343a37";
  context.fillRect(11 * TILE + 7, 5 * TILE + 2, 10, 15);
  context.fillStyle = "#ddd4ad";
  context.fillRect(12 * TILE, 5 * TILE + 3, 8, 11);
  context.fillStyle = "#525c56";
  context.fillRect(12 * TILE + 1, 5 * TILE + 5, 6, 1);
  context.fillRect(12 * TILE + 1, 5 * TILE + 8, 5, 1);
  context.fillRect(12 * TILE + 1, 5 * TILE + 11, 6, 1);
}

function drawDungeonApproach(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#59645f";
  context.fillRect(28 * TILE + 5, 2 * TILE, 5, 5 * TILE);
  context.fillRect(34 * TILE - 2, 2 * TILE, 5, 5 * TILE);
  context.fillRect(28 * TILE + 5, 2 * TILE, 6 * TILE - 2, 5);
  context.fillStyle = "#222a29";
  context.fillRect(30 * TILE, 2 * TILE + 5, 3 * TILE, 4 * TILE);
  context.fillStyle = "#aeb4a0";
  context.fillRect(30 * TILE + 3, 3 * TILE, 2, 3 * TILE);
  context.fillRect(31 * TILE + 3, 2 * TILE + 4, 2, 3 * TILE);
  context.fillRect(32 * TILE + 3, 3 * TILE, 2, 3 * TILE);
  context.fillStyle = "#c8b76f";
  context.fillRect(31 * TILE + 3, 5 * TILE + 3, 3, 3);
  drawLabel(context, "MYSTERY GATE", 28 * TILE, 7 * TILE);
}

function drawFallbackBuilding(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  label: string,
): void {
  context.fillStyle = "#3f4b46";
  context.fillRect(x, y + 10, width, height - 10);
  context.fillStyle = "#c9c3a4";
  context.fillRect(x + 4, y + 16, width - 8, height - 20);
  context.fillStyle = "#5b6661";
  context.fillRect(x - 2, y + 7, width + 4, 13);
  context.fillStyle = "#242a28";
  context.font = "bold 5px monospace";
  context.fillText(label, x + 5, y + 15);
}

function drawTownOrchard(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
): void {
  for (const [x, y] of TOWN_ORCHARD_TREES) {
    drawTreeAtTile(context, tileset, x, y, true);
    context.fillStyle = "#e5d3b2";
    context.fillRect(x * TILE - 6, y * TILE + 5, 2, 2);
    context.fillRect(x * TILE + 10, y * TILE + 2, 2, 2);
  }
  drawLabel(context, "SUNBERRY ORCHARD", 3 * TILE, 24 * TILE);
}

function drawTownOrchardFloor(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#97a779";
  fillTileRect(context, 2, 24, 17, 16);
  context.fillStyle = "#ddd4ad";
  context.fillRect(2 * TILE, 25 * TILE, 17 * TILE, 2);
  context.fillRect(2 * TILE, 39 * TILE, 17 * TILE, 2);
}

function drawTownScenery(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
): void {
  for (const [x, y] of TOWN_SCENERY_TREES) {
    drawTreeAtTile(context, tileset, x, y, false);
  }

  context.fillStyle = "#6f7f6a";
  for (let x = 36; x < 61; x += 4) {
    context.fillRect(x * TILE, 17 * TILE, 3 * TILE, 2);
  }
  drawFlowerPatch(context, 37, 19);
  drawFlowerPatch(context, 42, 23);
  drawFlowerPatch(context, 54, 27);

  context.fillStyle = "#655b4b";
  context.fillRect(40 * TILE, 9 * TILE + 2, 4 * TILE, 4);
  context.fillStyle = "#c4b38a";
  context.fillRect(40 * TILE + 2, 9 * TILE, 4 * TILE - 4, 4);
  context.fillRect(40 * TILE + 4, 9 * TILE + 6, 2, 5);
  context.fillRect(43 * TILE + 2, 9 * TILE + 6, 2, 5);
}

function drawFlowerPatch(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
): void {
  const colors = ["#d7c55d", "#d89077", "#ece5c6", "#8cae9b"];
  for (let index = 0; index < 8; index += 1) {
    context.fillStyle = colors[index % colors.length] ?? "#ece5c6";
    context.fillRect(
      tileX * TILE + (index * 7) % 31,
      tileY * TILE + (index * 5) % 19,
      2,
      2,
    );
  }
}

function drawTownBoundary(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
): void {
  const northGaps = new Set([12, 15, 18, 21, 29, 30, 31, 32, 33]);
  const southGaps = new Set([20, 24, 28, 32]);
  for (let x = 0; x < WORLD_SCENES.town.columns; x += 2) {
    const buildingFrontage =
      (x >= 2 && x <= 9) ||
      (x >= 23 && x <= 34) ||
      (x >= 52 && x <= 59);
    if (!northGaps.has(x) && !buildingFrontage) {
      drawTreeAtTile(context, tileset, x, 1, false);
    }
    if (!southGaps.has(x) && (x < 2 || x > 10)) {
      drawTreeAtTile(context, tileset, x, 47, false);
    }
  }
  context.fillStyle = "#53655b";
  for (let y = 2; y < 47; y += 2) {
    context.fillRect(0, y * TILE, 6, 9);
    context.fillRect(63 * TILE + 2, y * TILE, 6, 9);
  }
}

function drawTreeAtTile(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
  tileX: number,
  tileY: number,
  orchard: boolean,
): void {
  const width = 20;
  const height = 34;
  const x = tileX * TILE + Math.floor(TILE / 2) - Math.floor(width / 2);
  const y = (tileY + 1) * TILE - height;
  if (tileset) {
    context.drawImage(
      tileset,
      orchard ? 96 : 0,
      320,
      48,
      80,
      x,
      y,
      width,
      height,
    );
    return;
  }
  context.fillStyle = "#4c5d52";
  context.fillRect(x + 8, y + 19, 4, 14);
  context.fillStyle = "#71806b";
  context.fillRect(x + 2, y + 7, 16, 17);
  context.fillStyle = "#8d9976";
  context.fillRect(x + 5, y + 3, 10, 18);
  if (orchard) {
    context.fillStyle = "#d9b65d";
    context.fillRect(x + 5, y + 10, 2, 2);
    context.fillRect(x + 13, y + 15, 2, 2);
  }
}

function drawTreeCanopyAtTile(
  context: CanvasRenderingContext2D,
  tileset: HTMLImageElement | undefined,
  tileX: number,
  tileY: number,
  orchard: boolean,
): void {
  const width = 20;
  const height = 34;
  const x = tileX * TILE + Math.floor(TILE / 2) - Math.floor(width / 2);
  const y = (tileY + 1) * TILE - height;
  if (tileset) {
    context.drawImage(
      tileset,
      orchard ? 96 : 0,
      320,
      48,
      56,
      x,
      y,
      width,
      24,
    );
    return;
  }
  context.fillStyle = "#71806b";
  context.fillRect(x + 2, y + 7, 16, 17);
  context.fillStyle = "#8d9976";
  context.fillRect(x + 5, y + 3, 10, 18);
  if (orchard) {
    context.fillStyle = "#d9b65d";
    context.fillRect(x + 5, y + 10, 2, 2);
    context.fillRect(x + 13, y + 15, 2, 2);
  }
}

function drawTownProps(
  context: CanvasRenderingContext2D,
  state: WorldState,
): void {
  context.fillStyle = "#3d4642";
  context.fillRect(14 * TILE, 18 * TILE + 2, 5 * TILE, 5);
  context.fillStyle = "#b8aa86";
  context.fillRect(14 * TILE + 2, 18 * TILE, 5 * TILE - 4, 4);
  context.fillRect(14 * TILE + 4, 18 * TILE + 7, 2, 5);
  context.fillRect(18 * TILE + 2, 18 * TILE + 7, 2, 5);

  context.fillStyle = "#d8b85e";
  context.fillRect(16 * TILE + 1, 17 * TILE + 3, 7, 2);
  context.fillRect(16 * TILE - 1, 17 * TILE + 5, 11, 2);
  context.fillRect(16 * TILE + 3, 17 * TILE + 7, 3, 3);
  drawLabel(context, "VOICE RELAY", 14 * TILE, 19 * TILE + 5);

  if (state.mission.stage < 3) {
    context.fillStyle = "#222826";
    context.fillRect(10 * TILE + 1, 5 * TILE, 9, 9);
    context.fillStyle = "#f2eee0";
    context.fillRect(10 * TILE + 2, 5 * TILE + 1, 7, 7);
    context.fillStyle = "#222826";
    context.font = "bold 7px monospace";
    context.fillText("!", 10 * TILE + 4, 5 * TILE + 7);
  }
}

function drawGuildHall(
  context: CanvasRenderingContext2D,
  state: WorldState,
  reducedMotion: boolean,
): void {
  drawInteriorShell(context, "#6b746d", "#a89e82", "#c4b99a");

  context.fillStyle = "#8b765e";
  fillTileRect(context, 7, 3, 5, 4);
  context.fillStyle = "#403b35";
  context.fillRect(7 * TILE + 2, 3 * TILE + 2, 5 * TILE - 4, 4 * TILE - 4);
  context.fillStyle = "#d8d0ae";
  const notices = [
    [8, 4],
    [10, 4],
    [8, 6],
    [10, 5],
  ] as const;
  for (const [x, y] of notices) context.fillRect(x * TILE, y * TILE, 6, 5);
  drawLabel(
    context,
    "MISSION BOARD",
    WORLD_POIS.mission_board.x * TILE - 19,
    WORLD_POIS.mission_board.y * TILE + 2,
  );

  context.fillStyle = "#584f43";
  context.fillRect(13 * TILE, 5 * TILE - 2, 7 * TILE, 12);
  context.fillStyle = "#b3956e";
  context.fillRect(13 * TILE + 2, 5 * TILE, 7 * TILE - 4, 7);
  context.fillStyle = "#dfcf9f";
  context.fillRect(15 * TILE + 2, 5 * TILE + 2, 11, 3);
  context.fillStyle = "#655746";
  context.fillRect(14 * TILE, 6 * TILE + 2, 3, 4);
  context.fillRect(19 * TILE - 3, 6 * TILE + 2, 3, 4);
  drawLabel(context, "ROUTE TABLE", 14 * TILE, 7 * TILE);

  drawSupplyDepot(context, 5, 13, "SUPPLY DEPOT");
  drawSupplyDepot(context, 23, 13, "DISPATCH");
  drawGuildTotem(context, 4, 4);
  drawGuildTotem(context, 27, 4);

  context.fillStyle = "#8c6f57";
  context.fillRect(12 * TILE, 10 * TILE, 9 * TILE, 7 * TILE);
  context.fillStyle = "#c5b28a";
  context.fillRect(13 * TILE, 11 * TILE, 7 * TILE, 5 * TILE);
  context.fillStyle = "#887b68";
  for (let y = 12; y < 16; y += 2) {
    context.fillRect(14 * TILE, y * TILE, 5 * TILE, 1);
  }
  drawLabel(context, "ASSEMBLY RUG", 14 * TILE, 16 * TILE + 1);

  if (state.mission.stage < 3) {
    const pulse = reducedMotion ? 0 : Math.floor(state.elapsedMs / 240) % 2;
    context.strokeStyle = pulse === 0 ? "#fff0a5" : "#d1b85f";
    context.strokeRect(7 * TILE - pulse, 3 * TILE - pulse, 5 * TILE + pulse * 2, 4 * TILE + pulse * 2);
  }
}

function drawGuildHallForeground(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#584f43";
  context.fillRect(13 * TILE, 5 * TILE + 6, 7 * TILE, 4);
  context.fillStyle = "#b3956e";
  context.fillRect(13 * TILE + 2, 5 * TILE + 6, 7 * TILE - 4, 1);
  context.fillStyle = "#655746";
  context.fillRect(14 * TILE, 6 * TILE + 2, 3, 4);
  context.fillRect(19 * TILE - 3, 6 * TILE + 2, 3, 4);
}

function drawSupplyDepot(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
  label: string,
): void {
  context.fillStyle = "#514a40";
  fillTileRect(context, tileX, tileY, 4, 2);
  for (let x = tileX; x < tileX + 4; x += 1) {
    context.fillStyle = (x - tileX) % 2 === 0 ? "#b08c59" : "#967348";
    context.fillRect(x * TILE + 1, tileY * TILE + 1, 6, 14);
    context.fillStyle = "#e0c78e";
    context.fillRect(x * TILE + 2, tileY * TILE + 3, 4, 1);
    context.fillRect(x * TILE + 3, tileY * TILE + 8, 2, 3);
  }
  drawLabel(context, label, tileX * TILE - 2, (tileY + 2) * TILE + 1);
}

function drawGuildTotem(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
): void {
  context.fillStyle = "#3f4744";
  context.fillRect(tileX * TILE + 2, tileY * TILE, 5, 8);
  context.fillStyle = "#c7b66e";
  context.fillRect(tileX * TILE + 3, tileY * TILE + 1, 3, 3);
  context.fillStyle = "#68766d";
  context.fillRect(tileX * TILE + 1, tileY * TILE + 7, 7, 2);
}

function drawTrailShop(
  context: CanvasRenderingContext2D,
  state: WorldState,
  reducedMotion: boolean,
): void {
  drawInteriorShell(context, "#6d655b", "#a98f72", "#c9ad85");

  context.fillStyle = "#58493a";
  context.fillRect(11 * TILE, 7 * TILE - 2, 11 * TILE, 12);
  context.fillStyle = "#c29c65";
  context.fillRect(11 * TILE + 2, 7 * TILE, 11 * TILE - 4, 7);
  context.fillStyle = "#e3cf9c";
  context.fillRect(15 * TILE + 2, 7 * TILE + 1, 10, 3);
  context.fillStyle = "#3b3c36";
  context.fillRect(19 * TILE, 7 * TILE - 5, 8, 6);
  context.fillStyle = "#d5b75d";
  context.fillRect(19 * TILE + 2, 7 * TILE - 3, 4, 2);
  drawLabel(context, "TRAIL COUNTER", 13 * TILE, 9 * TILE);

  drawShopShelves(context, 5, 5);
  drawShopShelves(context, 24, 5);
  drawStockCrate(context, 5, 17, "BERRIES");
  drawStockCrate(context, 26, 17, "PACKS");

  context.fillStyle = "#75644f";
  fillTileRect(context, 12, 11, 9, 7);
  context.fillStyle = "#bca47e";
  fillTileRect(context, 13, 12, 7, 5);
  context.fillStyle = "#d8c99f";
  context.fillRect(15 * TILE, 14 * TILE + 3, 3 * TILE, 2);
  drawLabel(
    context,
    "STOCK & EXPEDITION GOODS",
    WORLD_POIS.shop.x * TILE - 48,
    18 * TILE + 2,
  );

  const pulse = reducedMotion ? 0 : Math.floor(state.elapsedMs / 300) % 2;
  context.fillStyle = pulse === 0 ? "#dabb68" : "#f0d98b";
  context.fillRect(16 * TILE + 2, 6 * TILE + 2, 4, 4);
}

function drawTrailShopForeground(context: CanvasRenderingContext2D): void {
  context.fillStyle = "#58493a";
  context.fillRect(11 * TILE, 7 * TILE + 6, 11 * TILE, 4);
  context.fillStyle = "#c29c65";
  context.fillRect(11 * TILE + 2, 7 * TILE + 6, 11 * TILE - 4, 1);
  drawShopShelfForeground(context, 5, 5);
  drawShopShelfForeground(context, 24, 5);
}

function drawShopShelfForeground(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
): void {
  for (let row = 0; row < 4; row += 1) {
    const y = (tileY + row * 2) * TILE;
    context.fillStyle = "#51463c";
    context.fillRect(tileX * TILE, y + 12, 3 * TILE, 2);
    context.fillStyle = "#a58155";
    context.fillRect(tileX * TILE + 2, y + 11, 3 * TILE - 4, 2);
  }
}

function drawShopShelves(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
): void {
  context.fillStyle = "#51463c";
  fillTileRect(context, tileX, tileY, 3, 8);
  for (let row = 0; row < 4; row += 1) {
    const y = (tileY + row * 2) * TILE;
    context.fillStyle = "#a58155";
    context.fillRect(tileX * TILE + 2, y + 2, 3 * TILE - 4, 12);
    context.fillStyle = "#e4cc8b";
    context.fillRect(tileX * TILE + 4, y + 4, 4, 5);
    context.fillStyle = row % 2 === 0 ? "#8ba18b" : "#bc8066";
    context.fillRect((tileX + 1) * TILE + 3, y + 5, 5, 5);
  }
}

function drawStockCrate(
  context: CanvasRenderingContext2D,
  tileX: number,
  tileY: number,
  label: string,
): void {
  context.fillStyle = "#765b3f";
  context.fillRect(tileX * TILE, tileY * TILE, TILE, TILE);
  context.fillStyle = "#c39b62";
  context.fillRect(tileX * TILE + 1, tileY * TILE + 1, 6, 6);
  context.fillStyle = "#594738";
  context.fillRect(tileX * TILE + 3, tileY * TILE + 1, 2, 6);
  drawLabel(context, label, tileX * TILE - 5, (tileY + 1) * TILE + 2);
}

function drawInteriorShell(
  context: CanvasRenderingContext2D,
  wall: string,
  floorDark: string,
  floorLight: string,
): void {
  context.fillStyle = "#252927";
  context.fillRect(0, 0, WORLD_PIXEL_WIDTH, WORLD_PIXEL_HEIGHT);
  context.fillStyle = floorDark;
  fillTileRect(context, 1, 1, 30, 22);
  for (let y = 1; y < 23; y += 1) {
    for (let x = 1; x < 31; x += 1) {
      if ((x + y) % 2 === 0) {
        context.fillStyle = floorLight;
        context.fillRect(x * TILE + 1, y * TILE + 1, 6, 6);
      }
    }
  }
  context.fillStyle = wall;
  context.fillRect(0, 0, WORLD_PIXEL_WIDTH, TILE);
  context.fillRect(0, 0, TILE, WORLD_PIXEL_HEIGHT);
  context.fillRect(31 * TILE, 0, TILE, WORLD_PIXEL_HEIGHT);
  context.fillRect(0, 23 * TILE, WORLD_PIXEL_WIDTH, TILE);
  context.fillStyle = "#3f4742";
  context.fillRect(TILE, TILE, 30 * TILE, 2);
  context.fillRect(TILE, 22 * TILE + 6, 30 * TILE, 2);
  context.fillStyle = "#d8cfa9";
  context.fillRect(3 * TILE, 3, 5 * TILE, 2);
  context.fillRect(24 * TILE, 3, 5 * TILE, 2);
}

function drawScenePortals(
  context: CanvasRenderingContext2D,
  state: WorldState,
  scene: WorldSceneId,
  reducedMotion: boolean,
): void {
  const player = actorWorldPosition(state.actors.player);
  const phase = reducedMotion ? 0 : Math.floor(state.elapsedMs / 180) % 3;
  for (const portal of WORLD_PORTALS) {
    if (portal.from.scene !== scene) continue;
    const { x, y } = portal.from;
    const near =
      player.scene === scene && Math.hypot(player.x - x, player.y - y) <= 2.25;
    const pixelX = x * TILE;
    const pixelY = y * TILE;

    context.fillStyle = scene === "town" ? "#252b29" : "#3d3730";
    context.fillRect(pixelX, pixelY, TILE, TILE);
    context.fillStyle = near ? "#f5dfa0" : "#c9b77c";
    context.fillRect(pixelX + 1, pixelY + (scene === "town" ? 1 : 5), 6, 2);
    context.fillStyle = near ? "#f4efce" : "#8da397";
    context.fillRect(pixelX + 3, pixelY + 3, 2, 2);

    if (near) {
      context.strokeStyle = phase === 0 ? "#fff4b0" : "#d7bd6d";
      context.strokeRect(pixelX - 1 - phase, pixelY - 1 - phase, 10 + phase * 2, 10 + phase * 2);
      context.fillStyle = "#fff4b0";
      if (scene === "town") {
        context.fillRect(pixelX + 3, pixelY + 6 - phase, 2, 2);
      } else {
        context.fillRect(pixelX + 3, pixelY + phase, 2, 2);
      }
    }
  }
}

function drawSpeechWave(
  context: CanvasRenderingContext2D,
  state: WorldState,
  camera: WorldCamera,
  reducedMotion: boolean,
): void {
  const wave = state.speechWave;
  if (!wave) return;
  const age = state.elapsedMs - wave.issuedAtMs;
  if (age < 0 || age > 1_150) return;
  const playerPosition = actorWorldPosition(state.actors.player);
  if (playerPosition.scene !== camera.scene) return;
  const player = viewportRenderPoint(camera, state.actors.player);
  if (!player) return;
  const progress = reducedMotion ? 0.72 : Math.min(1, age / 900);
  const maxRadius = (wave.guildWide ? 28 : wave.radius) * TILE;
  const alpha = reducedMotion ? 0.3 : Math.max(0, 0.55 * (1 - age / 1_150));
  context.save();
  context.beginPath();
  context.rect(0, 0, WORLD_PIXEL_WIDTH, WORLD_PIXEL_HEIGHT);
  context.clip();
  context.strokeStyle = wave.guildWide
    ? `rgba(255, 244, 172, ${alpha})`
    : `rgba(241, 239, 218, ${alpha})`;
  context.lineWidth = wave.voice === "shout" ? 2 : 1;
  context.setLineDash(wave.voice === "whisper" ? [2, 2] : []);
  context.beginPath();
  context.arc(
    Math.round(player.x + TILE / 2),
    Math.round(player.y + TILE / 2),
    Math.max(2, maxRadius * progress),
    0,
    Math.PI * 2,
  );
  context.stroke();
  context.restore();
}

function isActorVisible(actor: WorldActor, camera: WorldCamera): boolean {
  if (actorWorldPosition(actor).scene !== camera.scene) return false;
  const point = actorRenderPoint(actor);
  return (
    point.x >= camera.x &&
    point.y >= camera.y &&
    point.x < camera.x + WORLD_VIEW_COLUMNS &&
    point.y < camera.y + WORLD_VIEW_ROWS
  );
}

function viewportRenderPoint(
  camera: WorldCamera,
  actor: WorldActor,
): Readonly<{ x: number; y: number }> | undefined {
  const point = actorRenderPoint(actor);
  return worldToViewport(camera, {
    scene: actorWorldPosition(actor).scene,
    x: point.x,
    y: point.y,
  });
}

function drawActor(
  context: CanvasRenderingContext2D,
  state: WorldState,
  camera: WorldCamera,
  actor: WorldActor,
  assets: WorldAssets | undefined,
  reducedMotion: boolean,
): void {
  const point = viewportRenderPoint(camera, actor);
  if (!point) return;
  const x = Math.round(point.x + TILE / 2);
  const y = Math.round(point.y + TILE / 2);
  const walking = actor.movement !== undefined;
  const bounce = reducedMotion
    ? 0
    : walking
      ? Math.floor(state.elapsedMs / 100) % 2
      : 0;
  context.fillStyle = "rgba(20, 26, 23, .28)";
  context.fillRect(x - 7, y + 4, 14, 3);

  const humanImage =
    actor.kind !== "monster" ? assets?.humans[actor.sprite] : undefined;
  const monsterImage =
    actor.kind === "monster" ? assets?.monsters[actor.sprite] : undefined;
  if (humanImage) {
    const frame = reducedMotion
      ? 1
      : walking
        ? Math.floor(state.elapsedMs / 115) % 4
        : 1;
    const row = directionRow(actor.direction);
    const size = actor.kind === "player" ? 20 : 18;
    context.drawImage(
      humanImage,
      frame * 32,
      row * 32,
      32,
      32,
      x - Math.floor(size / 2),
      y - size + 5 - bounce,
      size,
      size,
    );
  } else if (monsterImage) {
    const frame = reducedMotion
      ? 0
      : Math.floor(state.elapsedMs / (walking ? 125 : 220) + actor.sprite) % 4;
    const width = actor.sprite === 11 ? 27 : 21 + (actor.sprite % 4);
    const height = actor.sprite === 11 ? 23 : 22 + (actor.sprite % 3);
    context.save();
    context.translate(x, y - 3 - bounce);
    if (actor.direction === "left") context.scale(-1, 1);
    context.drawImage(
      monsterImage,
      frame * 96,
      0,
      96,
      96,
      -Math.floor(width / 2),
      -height + 7,
      width,
      height,
    );
    context.restore();
  } else {
    drawFallbackActor(context, actor, x, y - bounce);
  }

  if (actor.carrying) drawCargoMarker(context, actor.carrying, x + 7, y - 10);
  drawActorEffect(context, state, actor, x, y, reducedMotion);
}

function drawCargoMarker(
  context: CanvasRenderingContext2D,
  carrying: "sunberry" | "supply_pack",
  x: number,
  y: number,
): void {
  context.fillStyle = "#252b29";
  context.fillRect(x - 1, y - 1, 8, 8);
  if (carrying === "sunberry") {
    context.fillStyle = "#6d805c";
    context.fillRect(x + 3, y, 2, 2);
    context.fillStyle = "#e3b75c";
    context.fillRect(x + 1, y + 2, 5, 5);
    context.fillStyle = "#f2dc88";
    context.fillRect(x + 2, y + 3, 2, 2);
    return;
  }
  context.fillStyle = "#ba8b58";
  context.fillRect(x, y + 1, 7, 6);
  context.fillStyle = "#e0c18a";
  context.fillRect(x + 1, y + 2, 5, 2);
  context.fillStyle = "#5f5143";
  context.fillRect(x + 3, y + 1, 1, 6);
}

function drawActorEffect(
  context: CanvasRenderingContext2D,
  state: WorldState,
  actor: WorldActor,
  x: number,
  y: number,
  reducedMotion: boolean,
): void {
  if (actor.listenerPulse && actor.listenerPulse.untilMs > state.elapsedMs) {
    const remaining = actor.listenerPulse.untilMs - state.elapsedMs;
    const pulse = reducedMotion ? 0 : Math.floor(remaining / 180) % 2;
    context.strokeStyle = pulse === 0 ? "#fff3a6" : "#d8b85e";
    context.lineWidth = 1;
    context.strokeRect(
      x - 9 - pulse,
      y - 13 - pulse,
      18 + pulse * 2,
      20 + pulse * 2,
    );
  }
  if (!actor.effect || actor.effect.untilMs <= state.elapsedMs) return;

  const remaining = actor.effect.untilMs - state.elapsedMs;
  const progress = Math.max(0, Math.min(1, 1 - remaining / 950));
  const alpha = Math.max(0, 1 - progress);
  context.strokeStyle = actor.effect.kind === "splash"
    ? `rgba(176, 223, 226, ${alpha})`
    : actor.effect.kind === "train"
      ? `rgba(255, 221, 131, ${alpha})`
      : `rgba(245, 241, 220, ${alpha})`;
  context.strokeRect(
    Math.round(x - 9 - progress * 4),
    Math.round(y - 12 - progress * 4),
    Math.round(18 + progress * 8),
    Math.round(18 + progress * 8),
  );
  if (actor.effect.kind === "gather" || actor.effect.kind === "offer") {
    context.fillStyle = `rgba(244, 211, 113, ${alpha})`;
    context.fillRect(x - 11, y - 8, 2, 2);
    context.fillRect(x + 9, y - 13, 2, 2);
    context.fillRect(x + 5, y + 2, 2, 2);
  } else if (actor.effect.kind === "splash") {
    context.fillStyle = `rgba(180, 226, 229, ${alpha})`;
    context.fillRect(x - 8, y + 3, 4, 1);
    context.fillRect(x + 4, y + 1, 5, 1);
  }
}

function drawFallbackActor(
  context: CanvasRenderingContext2D,
  actor: WorldActor,
  x: number,
  y: number,
): void {
  context.fillStyle =
    actor.kind === "player"
      ? "#ece6ce"
      : actor.kind === "human"
        ? "#d9b987"
        : "#343d39";
  context.fillRect(x - 6, y - 10, 12, 14);
  context.fillStyle = actor.kind === "monster" ? "#d5cfb5" : "#343d39";
  context.fillRect(x - 3, y - 7, 3, 3);
  context.fillRect(x + 2, y - 7, 2, 2);
}

function drawAtmosphere(
  context: CanvasRenderingContext2D,
  state: WorldState,
  scene: WorldSceneId,
  reducedMotion: boolean,
): void {
  const hour = state.minuteOfDay / 60;
  const night =
    hour < 6
      ? 0.35
      : hour < 8
        ? (8 - hour) * 0.12
        : hour > 19
          ? Math.min(0.35, (hour - 19) * 0.1)
          : 0;
  if (night > 0) {
    const indoorScale = WORLD_SCENES[scene].indoors ? 0.22 : 1;
    context.fillStyle = `rgba(19, 25, 26, ${night * indoorScale})`;
    context.fillRect(0, 0, WORLD_PIXEL_WIDTH, WORLD_PIXEL_HEIGHT);
  }
  if (WORLD_SCENES[scene].indoors || state.weather !== "drizzle") return;

  context.strokeStyle = "rgba(235, 239, 232, .38)";
  context.lineWidth = 1;
  const shift = reducedMotion ? 0 : Math.floor(state.elapsedMs / 45) % 16;
  context.beginPath();
  for (let x = -16; x < WORLD_PIXEL_WIDTH + 16; x += 13) {
    for (let y = -16; y < WORLD_PIXEL_HEIGHT; y += 23) {
      const startY = (y + shift) % (WORLD_PIXEL_HEIGHT + 16);
      context.moveTo(x, startY);
      context.lineTo(x - 3, startY + 7);
    }
  }
  context.stroke();
}

function drawActorOverlay(
  context: CanvasRenderingContext2D,
  state: WorldState,
  camera: WorldCamera,
  actor: WorldActor,
): void {
  const point = viewportRenderPoint(camera, actor);
  if (!point) return;
  const x = Math.round(point.x + TILE / 2);
  const y = Math.round(point.y + TILE / 2);
  if (actor.listenerPulse && actor.listenerPulse.untilMs > state.elapsedMs) {
    context.font = "bold 5px monospace";
    const label = "HEARD";
    const width = Math.ceil(context.measureText(label).width) + 4;
    const labelX = Math.max(
      1,
      Math.min(WORLD_PIXEL_WIDTH - width - 1, x - Math.floor(width / 2)),
    );
    const labelY = Math.min(WORLD_PIXEL_HEIGHT - 9, y + 6);
    context.fillStyle = "#222826";
    context.fillRect(labelX, labelY, width, 8);
    context.fillStyle = "#fff3a6";
    context.fillText(label, labelX + 2, labelY + 6);
  }
  if (actor.emote && actor.emote.untilMs > state.elapsedMs) {
    const text = emoteGlyph(actor.emote.icon);
    const emoteX = Math.max(6, Math.min(WORLD_PIXEL_WIDTH - 6, x));
    const emoteY = Math.max(12, y - 29);
    context.fillStyle = "#222826";
    context.fillRect(emoteX - 6, emoteY, 12, 11);
    context.fillStyle = "#f1ecda";
    context.fillRect(emoteX - 5, emoteY + 1, 10, 9);
    context.fillStyle = "#222826";
    context.font = "bold 7px monospace";
    context.fillText(
      text,
      emoteX - Math.ceil(context.measureText(text).width / 2),
      emoteY + 8,
    );
  }
  if (actor.bubble && actor.bubble.untilMs > state.elapsedMs) {
    drawSpeechBubble(context, actor.bubble.text, x, y - 23);
  }
}

function drawSpeechBubble(
  context: CanvasRenderingContext2D,
  text: string,
  centerX: number,
  bottomY: number,
): void {
  context.font = "5px monospace";
  const lines = wrapText(context, text.toUpperCase(), 25, 88).slice(0, 3);
  const width =
    Math.max(
      ...lines.map((line) => Math.ceil(context.measureText(line).width)),
      18,
    ) + 8;
  const height = lines.length * 7 + 6;
  const x = Math.max(
    2,
    Math.min(WORLD_PIXEL_WIDTH - width - 2, centerX - Math.floor(width / 2)),
  );
  const y = Math.max(
    2,
    Math.min(WORLD_PIXEL_HEIGHT - height - 4, bottomY - height),
  );
  context.fillStyle = "#222826";
  context.fillRect(x - 1, y - 1, width + 2, height + 2);
  context.fillStyle = "#f4efdc";
  context.fillRect(x, y, width, height);
  context.fillRect(
    Math.max(x + 3, Math.min(x + width - 6, centerX - 2)),
    y + height,
    4,
    3,
  );
  context.fillStyle = "#222826";
  lines.forEach((line, index) =>
    context.fillText(line, x + 4, y + 7 + index * 7),
  );
}

function drawScenePlaque(
  context: CanvasRenderingContext2D,
  scene: WorldSceneId,
): void {
  const label = WORLD_SCENES[scene].label.toUpperCase();
  context.font = "bold 5px monospace";
  const width = Math.ceil(context.measureText(label).width) + 8;
  context.fillStyle = "rgba(24, 29, 27, .88)";
  context.fillRect(3, 3, width, 10);
  context.fillStyle = "#d9c678";
  context.fillRect(3, 3, 2, 10);
  context.fillStyle = "#f1ecd5";
  context.fillText(label, 8, 10);
}

function drawLabel(
  context: CanvasRenderingContext2D,
  text: string,
  x: number,
  y: number,
): void {
  context.font = "bold 5px monospace";
  const width = Math.ceil(context.measureText(text).width) + 4;
  context.fillStyle = "rgba(25, 30, 28, .82)";
  context.fillRect(Math.round(x), Math.round(y), width, 8);
  context.fillStyle = "#eee8cf";
  context.fillText(text, Math.round(x) + 2, Math.round(y) + 6);
}

function fillTileRect(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
): void {
  context.fillRect(x * TILE, y * TILE, width * TILE, height * TILE);
}

function wrapText(
  context: CanvasRenderingContext2D,
  text: string,
  maxCharacters: number,
  maxWidth: number,
): string[] {
  const words = text.split(" ");
  const lines: string[] = [];
  let line = "";
  for (const word of words) {
    const candidate = line ? `${line} ${word}` : word;
    if (
      candidate.length <= maxCharacters &&
      context.measureText(candidate).width <= maxWidth
    ) {
      line = candidate;
      continue;
    }
    if (line) lines.push(line);
    line = word.slice(0, maxCharacters);
  }
  if (line) lines.push(line);
  return lines.length ? lines : [""];
}

function directionRow(direction: Direction): number {
  if (direction === "down") return 0;
  if (direction === "left") return 1;
  if (direction === "right") return 2;
  return 3;
}

function emoteGlyph(icon: string): string {
  if (icon === "heart") return "♥";
  if (icon === "music") return "♪";
  if (icon === "spark") return "✦";
  if (icon === "sweat") return "'";
  return icon;
}

async function loadImage(source: string): Promise<HTMLImageElement | undefined> {
  const image = new Image();
  image.src = source;
  try {
    await image.decode();
    return image;
  } catch {
    return undefined;
  }
}
