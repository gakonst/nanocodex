import type {
  Browser as BrowserApiBrowser,
  Browsers as BrowserApiBrowsers,
  Tab as BrowserApiTab,
} from "./browser-api.d.mts";

export type Vec2 = [x: number, y: number];
export type Point = Vec2;
export type DesktopPoint = { x: number; y: number };
export type ObservationOptions = { emit?: boolean };
export type StateOptions = ObservationOptions & { disableDiffing?: boolean };
export type StateAndScreenshot = { state: string; screenshot?: Uint8Array };
export type PasteOptions = { format?: "text" | "md" | "html" };
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
  click(target: number | Vec2, options?: CuaClickOptions): Promise<void>;
  drag(from: Vec2, to: Vec2): Promise<void>;
  scroll(target: number | Vec2, direction: Direction, pages?: number): Promise<void>;
  selectText(elementIndex: number, text: string, options?: SelectTextOptions): Promise<void>;
  setValue(elementIndex: number, value: string): Promise<void>;
  performSecondaryAction(elementIndex: number, action: string): Promise<void>;
}

export type AppInfo = {
  id: string;
  displayName?: string;
  lastUsedDate?: string;
  useCount?: number;
  isRunning?: boolean;
  windows?: WindowInfo[];
};
export interface WindowInfo { id: number; app: string; title?: string; }
export type AppReference = string | { windowId: number };
export interface App extends Target {
  scroll(target: number | Vec2, direction: Direction, distance?: number | { pixels: number }): Promise<void>;
  paste(text: string, options?: PasteOptions): Promise<void>;
  pressKey(key: string): Promise<void>;
  typeText(text: string): Promise<void>;
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
export type ComputerState = { apps: AppInfo[]; browsers: BrowserState[]; errors?: string[] };
export type TabInfo = BrowserTabInfo & { browserId: string };
export type BrowserOptions = { browser?: string };
export type GetBrowserOptions = { id?: string; extensionInstanceId?: string; url?: string };
export type TabReference = string | { mention: string } | { url: string };
export type CreateBrowserTabOptions = { visible?: boolean; sessionName?: string };

export interface Browser extends BrowserApiBrowser {
  tabs: Omit<BrowserApiBrowser["tabs"], "get" | "new" | "selected"> & {
    get(id: string): Promise<Tab>;
    new(): Promise<Tab>;
    selected(): Promise<Tab | undefined>;
  };
  user: Omit<BrowserApiBrowser["user"], "claimTab"> & {
    claimTab(tab: Parameters<BrowserApiBrowser["user"]["claimTab"]>[0]): Promise<Tab>;
  };
}
export interface BrowserProvider extends BrowserApiBrowsers {
  get(id: string): Promise<Browser>;
  getDefault(): Promise<Browser>;
  getForUrl(url: string): Promise<Browser>;
}
export interface Tab extends BrowserApiTab, Target {
  paste(elementIndex: number | null, text: string, options?: PasteOptions): Promise<void>;
  pressKey(elementIndex: number | null, key: string): Promise<void>;
  typeText(elementIndex: number | null, text: string): Promise<void>;
}

export type Screenshot = {
  bytes: Uint8Array;
  data_url: string;
  filepath: string;
};
export interface DragHandle {
  start(point: DesktopPoint): Promise<void>;
  move_to(point: DesktopPoint): Promise<void>;
  end(): Promise<void>;
}
export interface Computer {
  readonly target: "linux" | "mac" | "windows";
  drag_handle?(): DragHandle;
  get_screenshot?(): Promise<Screenshot[]>;
  move?(point: DesktopPoint): Promise<void>;
}

export interface Cua {
  initialize(): Promise<ComputerState>;
  /** Redisplay documentation emitted in this session. */
  rewriteDocumentation?(): Promise<void>;
  getState?(options?: ObservationOptions): Promise<ComputerState>;
  browsers?: BrowserProvider;
  computer?: Computer;
  getApp?(target: AppReference): Promise<App>;
  listWindows?(options?: ObservationOptions): Promise<WindowInfo[]>;
  listApps?(options?: ObservationOptions): Promise<AppInfo[]>;
  getBrowser?(options?: GetBrowserOptions): Promise<Browser>;
  createBrowserTab?(
    browserId: string,
    url?: string,
    options?: CreateBrowserTabOptions,
  ): Promise<Tab>;
  getTab?(reference: TabReference, options?: BrowserOptions): Promise<Tab>;
  listBrowsers?(options?: ObservationOptions): Promise<BrowserInfo[]>;
  listTabs?(options?: BrowserOptions & ObservationOptions): Promise<TabInfo[]>;
}

export type TinySkyAlt = Cua;
export type State = ComputerState;
export type SetupOptions = { browser?: boolean; computer?: boolean };
export type ClickOptions = CuaClickOptions;

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
