import type {
  Browser as BrowserApiBrowser,
  Browsers as BrowserApiBrowsers,
  Tab as BrowserApiTab,
} from "./browser-api.d.mts";

export type Vec2 = [x: number, y: number];
export type Point = { x: number; y: number };
export type ObservationOptions = { emit?: boolean };
export type StateOptions = ObservationOptions & { disableDiffing?: boolean };
export type StateAndScreenshot = { state: string; screenshot?: Uint8Array };
export type PasteOptions = { format?: "text" | "md" | "html" };
export type CuaDragOptions = { mouseButton?: MouseButton; modifiers?: ("shift" | "ctrl" | "control" | "alt" | "option" | "super" | "meta" | "cmd" | "command")[] };
export type CuaClickOptions = { mouseButton?: MouseButton; clickCount?: number };
export type SelectTextOptions = {
  prefix?: string;
  suffix?: string;
  selectionType?: SelectionType;
};
export type Direction = "up" | "down" | "left" | "right" | "u" | "d" | "l" | "r";
export type SelectionType = "text" | "cursor_before" | "cursor_after";
export type MouseButton = "left" | "right" | "middle" | "l" | "r" | "m";

export interface Target {
  getAXState(options?: StateOptions): Promise<string>;
  getScreenshot(options?: ObservationOptions): Promise<Uint8Array>;
  getAXStateAndScreenshot(options?: StateOptions): Promise<StateAndScreenshot>;
  paste(text: string, options?: PasteOptions): Promise<void>;
  click(target: number | Vec2, options?: CuaClickOptions): Promise<void>;
  drag(from: Vec2, to: Vec2): Promise<void>;
  pressKey(key: string): Promise<void>;
  scroll(target: number | Vec2, direction: Direction, pages?: number): Promise<void>;
  selectText(elementIndex: number, text: string, options?: SelectTextOptions): Promise<void>;
  setValue(elementIndex: number, value: string): Promise<void>;
  typeText(text: string): Promise<void>;
  performSecondaryAction(elementIndex: number, action: string): Promise<void>;
}

export type AppInfo = {
  id: string;
  displayName?: string;
  lastUsedDate?: string;
  useCount?: number;
  isRunning?: boolean;
};
export type AppWindowInfo = {
  windowId: number;
  pid: number;
  title?: string | null;
  frame?: [x: number, y: number, width: number, height: number] | null;
};
export type GetAppOptions = { windowId?: number };
export interface App extends Target {
  /** Native gesture; unsupported backends refuse before input. */
  drag(from: Vec2, to: Vec2, options?: CuaDragOptions): Promise<void>;
}

export type BrowserInfo = {
  id: string;
  name?: string;
  family?: string;
  type?: "iab" | "extension" | "cdp";
  profileName?: string;
  metadata?: { extensionInstanceId?: string; codexSessionId?: string };
};
export type BrowserTabInfo = {
  id: string;
  providerTabId?: string;
  title?: string;
  url?: string;
};
export type BrowserState = BrowserInfo & { tabs: BrowserTabInfo[] };
export type ComputerState = { apps: AppInfo[]; browsers: BrowserState[] };
export type TabInfo = BrowserTabInfo & { browserId: string };
export type BrowserOptions = { browser?: string };
export type GetBrowserOptions = { id?: string; url?: string };
export type CreateBrowserTabOptions = { visible?: boolean; sessionName?: string };

export interface Browser extends BrowserApiBrowser {}
export interface BrowserProvider extends BrowserApiBrowsers {}
export interface Tab extends BrowserApiTab, Target {}

export type Screenshot = {
  bytes: Uint8Array;
  data_url: string;
  filepath: string;
};
export interface DragHandle {
  start(point: Point): Promise<void>;
  move_to(point: Point): Promise<void>;
  end(): Promise<void>;
}
export interface Computer {
  readonly target: "linux" | "mac" | "windows";
  drag_handle?(): DragHandle;
  get_screenshot?(): Promise<Screenshot[]>;
  /** macOS main display; read-only, without app coordinate authority. */
  get_desktop_screenshot?(): Promise<Uint8Array>;
  move?(point: Point): Promise<void>;
}

export interface Cua {
  initialize(): Promise<ComputerState>;
  getState(options?: ObservationOptions): Promise<ComputerState>;
  /** macOS main display; present when supported by the native provider. */
  getScreenshot?(options?: ObservationOptions): Promise<Uint8Array>;
  readonly browsers: BrowserProvider;
  readonly computer: Computer;
  /** Bind an exact window from listWindows for independent background control. */
  getApp(app: string, options?: GetAppOptions): Promise<App>;
  /** Present when the backend supports explicit native-window discovery. */
  listWindows?(app: string, options?: ObservationOptions): Promise<AppWindowInfo[]>;
  listApps(options?: ObservationOptions): Promise<AppInfo[]>;
  getBrowser(options?: GetBrowserOptions): Promise<Browser>;
  createBrowserTab(
    browserId: string,
    url?: string,
    options?: CreateBrowserTabOptions,
  ): Promise<Tab>;
  getTab(id: string, options?: BrowserOptions): Promise<Tab>;
  listBrowsers(options?: ObservationOptions): Promise<BrowserInfo[]>;
  listTabs(options?: BrowserOptions & ObservationOptions): Promise<TabInfo[]>;
}

export type ImageInput =
  | string
  | ArrayBuffer
  | ArrayBufferView
  | Uint8Array
  | { bytes: ArrayBuffer | ArrayBufferView | Uint8Array; mimeType: string };
export interface NodeRepl {
  readonly cwd: string;
  readonly homeDir: string;
  readonly tmpDir: string;
  readonly env: Readonly<Record<string, string>>;
  readonly requestMeta: Readonly<Record<string, unknown>>;
  write(value: unknown, itemId?: string): void;
  emitImage(value: ImageInput | PromiseLike<ImageInput>): Promise<void>;
  rpc(service: string, request: unknown): Promise<unknown>;
}
export interface CuaGlobals {
  cua: Cua;
  nodeRepl: NodeRepl;
}

export type * from "./browser-api.d.mts";
