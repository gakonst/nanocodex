import type { AXDirection, AXMouseButton, AXPoint, AXSelectionType, BrowserTab, GlobalAgentApi } from "@oai/browser";
import type { sky } from "@oai/sky";
export type { BrowserTab } from "@oai/browser";
export type NodeReplGlobals = typeof globalThis & {
    nodeRepl?: {
        env?: Record<string, string | undefined>;
        requestMeta?: Record<string, unknown>;
        emitImage?: (image: {
            bytes: Uint8Array;
            mimeType: string;
        }) => Promise<void> | void;
        write?: (text: string, itemId?: string) => Promise<void> | void;
    };
};
export interface SetupOptions {
    browser?: boolean;
    computer?: boolean;
}
export type Browsers = GlobalAgentApi<Tab>["browsers"];
export type Browser = Awaited<ReturnType<Browsers["get"]>>;
export type BrowserInfo = Awaited<ReturnType<Browsers["list"]>>[number];
export type TabInfo = Awaited<ReturnType<Browser["tabs"]["list"]>>[number] & {
    browserId: string;
};
export type Computer = typeof sky;
export type LinuxComputer = Extract<Computer, {
    target: "linux";
}>;
export type MacComputer = Extract<Computer, {
    target: "mac";
}>;
export type WindowsComputer = Extract<Computer, {
    target: "windows";
}>;
export type Point = AXPoint;
export type Direction = AXDirection;
export type MouseButton = AXMouseButton;
export type SelectionType = AXSelectionType;
export interface ObservationOptions {
    /** Display the observation to the model. Defaults to true. */
    emit?: boolean;
}
export interface StateOptions extends ObservationOptions {
    disableDiffing?: boolean;
}
export interface ClickOptions {
    mouseButton?: MouseButton;
    clickCount?: number;
}
export interface SelectTextOptions {
    prefix?: string;
    suffix?: string;
    selectionType?: SelectionType;
}
export interface PasteOptions {
    format?: "text" | "md" | "html";
}
export interface BrowserOptions {
    browser?: string;
}
/** A runtime/provider tab ID, a complete tab-v1 mention, or an exact URL in a specified browser. */
export type TabReference = string | {
    mention: string;
} | {
    url: string;
};
export interface GetBrowserOptions {
    id?: string;
    extensionInstanceId?: string;
    url?: string;
}
export interface CreateBrowserTabOptions {
    visible?: boolean;
    sessionName?: string;
}
export interface StateAndScreenshot {
    state: string;
    screenshot?: Uint8Array;
}
/** The shared interaction surface of an explicitly bound app or tab. */
export interface Target {
    /** Display and return the current accessibility state. */
    getAXState(options?: StateOptions): Promise<string>;
    /** Display and return the screenshot. */
    getScreenshot(options?: ObservationOptions): Promise<Uint8Array>;
    /** Display and return accessibility state and its screenshot, when available. */
    getAXStateAndScreenshot(options?: StateOptions): Promise<StateAndScreenshot>;
    click(target: number | Point, options?: ClickOptions): Promise<void>;
    drag(from: Point, to: Point): Promise<void>;
    scroll(target: number | Point, direction: Direction, pages?: number): Promise<void>;
    selectText(elementIndex: number, text: string, options?: SelectTextOptions): Promise<void>;
    setValue(elementIndex: number, value: string): Promise<void>;
    performSecondaryAction(elementIndex: number, action: string): Promise<void>;
}
export interface App extends Target {
    /** macOS accepts pages. Linux and Windows accept { pixels }; Windows requires a point. */
    scroll(target: number | Point, direction: Direction, distance?: number | {
        pixels: number;
    }): Promise<void>;
    paste(text: string, options?: PasteOptions): Promise<void>;
    pressKey(key: string): Promise<void>;
    typeText(text: string): Promise<void>;
}
/** The original browser tab, augmented with the shared interaction surface. */
export type Tab = BrowserTab & Target & {
    /** Focus an editable, visible target within 250 ms before pasting; null uses current focus. */
    paste(elementIndex: number | null, text: string, options?: PasteOptions): Promise<void>;
    /** Focus the target within 250 ms before pressing a key; null uses current focus. */
    pressKey(elementIndex: number | null, key: string): Promise<void>;
    /** Focus an editable, visible target within 250 ms before typing; null uses current focus. */
    typeText(elementIndex: number | null, text: string): Promise<void>;
};
export interface AppInfo {
    id: string;
    displayName?: string;
    isRunning?: boolean;
    lastUsedDate?: string;
    useCount?: number;
    /** Open windows on Linux and Windows. Select one by its ID. */
    windows?: WindowInfo[];
}
export interface WindowInfo {
    id: number;
    app: string;
    title?: string;
}
/** A macOS app name, path, or bundle ID, or a Linux/Windows window ID from inventory. */
export type AppReference = string | {
    windowId: number;
};
export interface State {
    apps: AppInfo[];
    browsers: Array<BrowserInfo & {
        tabs: Awaited<ReturnType<Browser["tabs"]["list"]>>;
    }>;
    /** Inventory failures; the other inventory remains usable. */
    errors?: string[];
}
/** Only enabled providers and their methods are installed. */
export interface TinySkyAlt {
    /** Collect and display fresh app and browser/tab inventory. */
    initialize(): Promise<State>;
    /** Redisplay all documentation already emitted in this session. */
    rewriteDocumentation?(): Promise<void>;
    /** Collect and display fresh app and browser/tab inventory. */
    getState?(options?: ObservationOptions): Promise<State>;
    browsers?: Browsers;
    computer?: Computer;
    /** Select a browser and display its documentation without opening a tab. */
    getBrowser?(options?: GetBrowserOptions): Promise<Browser>;
    /** Apply browser settings, create a tab, and display its initial full accessibility state. */
    createBrowserTab?(browserId: string, url?: string, options?: CreateBrowserTabOptions): Promise<Tab>;
    /** Bind an existing tab. URL references require a browser and exactly one match; never opens a tab. */
    getTab?(reference: TabReference, options?: BrowserOptions): Promise<Tab>;
    listBrowsers?(options?: ObservationOptions): Promise<BrowserInfo[]>;
    /** List user and controlled tabs without claiming them; optionally scope to one browser. */
    listTabs?(options?: BrowserOptions & ObservationOptions): Promise<TabInfo[]>;
    /** Bind a native app and display its initial full accessibility state. */
    getApp?(target: AppReference): Promise<App>;
    listApps?(options?: ObservationOptions): Promise<AppInfo[]>;
    /** List open native windows on Linux and Windows, including windows without an app entry. */
    listWindows?(options?: ObservationOptions): Promise<WindowInfo[]>;
}
