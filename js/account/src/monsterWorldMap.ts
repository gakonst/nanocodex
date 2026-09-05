import {
  type ActorId,
  type WorldPosition,
  type WorldSceneId,
  type WorldTarget,
} from "./monsterWorldProtocol.ts";

export const WORLD_TILE_SIZE = 8;
export const WORLD_VIEW_COLUMNS = 32;
export const WORLD_VIEW_ROWS = 24;
export const WORLD_PIXEL_WIDTH = WORLD_VIEW_COLUMNS * WORLD_TILE_SIZE;
export const WORLD_PIXEL_HEIGHT = WORLD_VIEW_ROWS * WORLD_TILE_SIZE;

export type WorldScene = Readonly<{
  id: WorldSceneId;
  label: string;
  columns: number;
  rows: number;
  indoors: boolean;
}>;

export type WorldCamera = Readonly<{
  scene: WorldSceneId;
  x: number;
  y: number;
}>;

export type WorldPortal = Readonly<{
  id: string;
  pairId: string;
  from: WorldPosition;
  to: WorldPosition;
}>;

export type WorldEntryPortal = Readonly<{
  outside: WorldPosition;
  inside: WorldPosition;
  label: "west" | "east" | "north" | "south";
}>;

type PoiTarget = Exclude<WorldTarget, ActorId>;
type WorldPoi = WorldPosition & Readonly<{ label: string }>;

export const WORLD_SCENES = Object.freeze({
  town: Object.freeze({
    id: "town",
    label: "Springleaf District",
    columns: 64,
    rows: 48,
    indoors: false,
  }),
  guild_hall: Object.freeze({
    id: "guild_hall",
    label: "Rescue Guild Hall",
    columns: 32,
    rows: 24,
    indoors: true,
  }),
  trail_shop: Object.freeze({
    id: "trail_shop",
    label: "Trail Shop",
    columns: 32,
    rows: 24,
    indoors: true,
  }),
} satisfies Record<WorldSceneId, WorldScene>);

export const WORLD_POIS = Object.freeze({
  guild: poi("guild_hall", 16, 11, "Rescue Guild Hall"),
  mission_board: poi("guild_hall", 9, 7, "Mission Board"),
  plaza: poi("town", 16, 11, "Guild Plaza"),
  orchard: poi("town", 10, 30, "Sunberry Orchard"),
  pond: poi("town", 45, 30, "Whisper Pond"),
  shop: poi("trail_shop", 16, 10, "Trail Shop Counter"),
  meadow: poi("town", 26, 37, "Training Meadow"),
  bridge: poi("town", 52, 34, "Bell Bridge"),
  dungeon_gate: poi("town", 31, 5, "Mystery Gate"),
} satisfies Record<PoiTarget, WorldPoi>);

export const WORLD_PORTALS: readonly WorldPortal[] = Object.freeze([
  portal("town-guild-door", "guild-exit", position("town", 6, 7), position("guild_hall", 16, 20)),
  portal("guild-exit", "town-guild-door", position("guild_hall", 16, 22), position("town", 6, 8)),
  portal("town-shop-door", "shop-exit", position("town", 26, 7), position("trail_shop", 16, 20)),
  portal("shop-exit", "town-shop-door", position("trail_shop", 16, 22), position("town", 26, 8)),
]);

export const WORLD_ENTRY_PORTALS: readonly WorldEntryPortal[] = Object.freeze([
  ...Array.from({ length: 20 }, (_, index) => entry(
    position("town", -2, 3 + index),
    position("town", 1, 3 + index),
    "west",
  )),
  ...Array.from({ length: 20 }, (_, index) => entry(
    position("town", 66, 3 + index),
    position("town", 62, 3 + index),
    "east",
  )),
  ...[12, 15, 18, 21].map((x) => entry(
    position("town", x, -2),
    position("town", x, 2),
    "north",
  )),
  ...[20, 24, 28, 32].map((x) => entry(
    position("town", x, 50),
    position("town", x, 46),
    "south",
  )),
]);

const orchardTrees = Object.freeze([
  position("town", 4, 27), position("town", 8, 27), position("town", 12, 27), position("town", 16, 27),
  position("town", 4, 32), position("town", 8, 32), position("town", 12, 32), position("town", 16, 32),
  position("town", 4, 37), position("town", 8, 37), position("town", 12, 37), position("town", 16, 37),
]);

const townScenery = Object.freeze([
  ...rectanglePositions("town", 3, 41, 9, 45),
  ...rectanglePositions("town", 52, 5, 59, 10),
  position("town", 34, 17),
  position("town", 35, 17),
  position("town", 58, 22),
  position("town", 59, 22),
]);

const hallFurniture = Object.freeze([
  ...rectanglePositions("guild_hall", 13, 5, 19, 5),
  ...rectanglePositions("guild_hall", 5, 13, 8, 14),
  ...rectanglePositions("guild_hall", 23, 13, 26, 14),
  position("guild_hall", 4, 4),
  position("guild_hall", 27, 4),
]);

const shopFurniture = Object.freeze([
  ...rectanglePositions("trail_shop", 11, 7, 21, 7),
  ...rectanglePositions("trail_shop", 5, 5, 7, 12),
  ...rectanglePositions("trail_shop", 24, 5, 26, 12),
  position("trail_shop", 5, 17),
  position("trail_shop", 26, 17),
]);

// Routing probes the same static collision map thousands of times during a
// cold simulation tick. Keep those probes numeric and allocation-free instead
// of rescanning position objects for every BFS neighbor.
const townStaticBlocks = new Set([
  ...orchardTrees,
  ...townScenery,
].map((point) => tileIndex(point.x, point.y, WORLD_SCENES.town.columns)));
const hallStaticBlocks = new Set(hallFurniture.map((point) => (
  tileIndex(point.x, point.y, WORLD_SCENES.guild_hall.columns)
)));
const shopStaticBlocks = new Set(shopFurniture.map((point) => (
  tileIndex(point.x, point.y, WORLD_SCENES.trail_shop.columns)
)));

const portalByPosition = new Map(WORLD_PORTALS.map((worldPortal) => [
  positionKey(worldPortal.from),
  worldPortal,
]));

export function isWorldPositionInBounds(value: WorldPosition): boolean {
  const scene = (WORLD_SCENES as Partial<Record<string, WorldScene>>)[value.scene];
  if (!scene) return false;
  return Number.isInteger(value.x)
    && Number.isInteger(value.y)
    && value.x >= 0
    && value.y >= 0
    && value.x < scene.columns
    && value.y < scene.rows;
}

export function isWorldPositionBlocked(value: WorldPosition): boolean {
  if (!isWorldPositionInBounds(value)) return true;
  if (value.scene === "town") return townBlocked(value.x, value.y);
  if (value.x < 1 || value.y < 1 || value.x >= 31 || value.y >= 23) return true;
  const furniture = value.scene === "guild_hall" ? hallStaticBlocks : shopStaticBlocks;
  return furniture.has(tileIndex(value.x, value.y, WORLD_SCENES[value.scene].columns));
}

export function isBlocked(x: number, y: number, scene: WorldSceneId = "town"): boolean {
  return isWorldPositionBlocked({ scene, x, y });
}

export function portalDestinationAt(value: WorldPosition): WorldPosition | undefined {
  const destination = portalByPosition.get(positionKey(value))?.to;
  return destination === undefined ? undefined : Object.freeze({ ...destination });
}

export function findWorldRoute(
  start: WorldPosition,
  goal: WorldPosition,
  temporarilyBlocked?: (position: WorldPosition) => boolean,
): readonly WorldPosition[] {
  if (isWorldPositionBlocked(start) || isWorldPositionBlocked(goal)) return Object.freeze([]);
  if (samePosition(start, goal)) return Object.freeze([]);

  const startKey = positionKey(start);
  const queue: WorldPosition[] = [Object.freeze({ ...start })];
  const previous = new Map<string, string | undefined>([[startKey, undefined]]);
  const points = new Map<string, WorldPosition>([[startKey, queue[0] as WorldPosition]]);
  let foundKey: string | undefined;

  for (let index = 0; index < queue.length; index += 1) {
    const current = queue[index];
    if (!current) continue;
    for (const next of routeNeighbors(current)) {
      const key = positionKey(next);
      if (
        previous.has(key)
        || isWorldPositionBlocked(next)
        || (!samePosition(next, goal) && temporarilyBlocked?.(next) === true)
      ) continue;
      previous.set(key, positionKey(current));
      points.set(key, next);
      if (samePosition(next, goal)) {
        foundKey = key;
        index = queue.length;
        break;
      }
      queue.push(next);
    }
  }
  if (!foundKey) return Object.freeze([]);

  const route: WorldPosition[] = [];
  let cursor: string | undefined = foundKey;
  while (cursor && cursor !== startKey) {
    const point = points.get(cursor);
    if (!point) return Object.freeze([]);
    route.push(Object.freeze({ ...point }));
    cursor = previous.get(cursor);
  }
  route.reverse();
  return Object.freeze(route);
}

export function cameraForPosition(player: WorldPosition): WorldCamera {
  const scene = WORLD_SCENES[player.scene];
  const maxX = Math.max(0, scene.columns - WORLD_VIEW_COLUMNS);
  const maxY = Math.max(0, scene.rows - WORLD_VIEW_ROWS);
  return Object.freeze({
    scene: player.scene,
    x: clamp(Math.floor(player.x - WORLD_VIEW_COLUMNS / 2), 0, maxX),
    y: clamp(Math.floor(player.y - WORLD_VIEW_ROWS / 2), 0, maxY),
  });
}

export function viewportToWorld(
  camera: WorldCamera,
  viewportPixelX: number,
  viewportPixelY: number,
): WorldPosition {
  return Object.freeze({
    scene: camera.scene,
    x: camera.x + Math.floor(viewportPixelX / WORLD_TILE_SIZE),
    y: camera.y + Math.floor(viewportPixelY / WORLD_TILE_SIZE),
  });
}

export function worldToViewport(
  camera: WorldCamera,
  value: WorldPosition,
): Readonly<{ x: number; y: number }> | undefined {
  if (camera.scene !== value.scene) return undefined;
  return Object.freeze({
    x: (value.x - camera.x) * WORLD_TILE_SIZE,
    y: (value.y - camera.y) * WORLD_TILE_SIZE,
  });
}

export function sceneLabel(scene: WorldSceneId): string {
  return WORLD_SCENES[scene].label;
}

function routeNeighbors(current: WorldPosition): readonly WorldPosition[] {
  const cardinal = [
    position(current.scene, current.x, current.y - 1),
    position(current.scene, current.x - 1, current.y),
    position(current.scene, current.x + 1, current.y),
    position(current.scene, current.x, current.y + 1),
  ];
  const portal = portalDestinationAt(current);
  return portal === undefined ? cardinal : [...cardinal, portal];
}

function townBlocked(x: number, y: number): boolean {
  if (x < 1 || y < 2 || x >= 63 || y >= 47) return true;
  if (inside(x, y, 2, 2, 9, 6)) return true;
  if (inside(x, y, 23, 2, 29, 6)) return true;
  if (inside(x, y, 46, 31, 62, 46) && y !== 34) return true;
  return townStaticBlocks.has(tileIndex(x, y, WORLD_SCENES.town.columns));
}

function tileIndex(x: number, y: number, columns: number): number {
  return y * columns + x;
}

function poi(scene: WorldSceneId, x: number, y: number, label: string): WorldPoi {
  return Object.freeze({ scene, x, y, label });
}

function portal(
  id: string,
  pairId: string,
  from: WorldPosition,
  to: WorldPosition,
): WorldPortal {
  return Object.freeze({ id, pairId, from, to });
}

function entry(
  outside: WorldPosition,
  insidePosition: WorldPosition,
  label: WorldEntryPortal["label"],
): WorldEntryPortal {
  return Object.freeze({ outside, inside: insidePosition, label });
}

function position(scene: WorldSceneId, x: number, y: number): WorldPosition {
  return Object.freeze({ scene, x, y });
}

function rectanglePositions(
  scene: WorldSceneId,
  left: number,
  top: number,
  right: number,
  bottom: number,
): WorldPosition[] {
  const points: WorldPosition[] = [];
  for (let y = top; y <= bottom; y += 1) {
    for (let x = left; x <= right; x += 1) points.push(position(scene, x, y));
  }
  return points;
}

function samePosition(left: WorldPosition, right: WorldPosition): boolean {
  return left.scene === right.scene && left.x === right.x && left.y === right.y;
}

function positionKey(value: WorldPosition): string {
  return `${value.scene}:${value.x},${value.y}`;
}

function inside(x: number, y: number, left: number, top: number, right: number, bottom: number): boolean {
  return x >= left && x <= right && y >= top && y <= bottom;
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.max(minimum, Math.min(maximum, value));
}
