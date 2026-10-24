import { afterEach, beforeEach, expect, jest, mock, test } from "bun:test";

const requests: unknown[] = [];
const activeDisposers: Array<() => void> = [];
let importCounter = 0;
let holdConnections = false;
let failConnections = false;
const connections: Array<() => void> = [];

mock.module("node:net", () => ({
  default: {
    createConnection(_path: string, onConnect: () => void) {
      const handlers = new Map<string, () => void>();
      const client = {
        destroyed: false,
        write(input: string) {
          if (client.destroyed) return;
          const request = JSON.parse(input.trim());
          requests.push(request);
          queueMicrotask(() => client.emit("data"));
        },
        setTimeout() {},
        on(event: string, handler: () => void) {
          handlers.set(event, handler);
        },
        destroy() {
          client.destroyed = true;
        },
        emit(event: string) {
          handlers.get(event)?.();
        },
      };
      if (holdConnections) connections.push(onConnect);
      else if (failConnections) queueMicrotask(() => client.emit("error"));
      else queueMicrotask(onConnect);
      return client;
    },
  },
}));

beforeEach(() => {
  // The plugin schedules its retries from Date.now() and timers; real time
  // would let a stalled runner move a deadline across the test's waits.
  jest.useFakeTimers();
  requests.length = 0;
  holdConnections = false;
  failConnections = false;
  connections.length = 0;
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = "test.sock";
  process.env.HERDR_PANE_ID = "test:p1";
});

afterEach(() => {
  for (const dispose of activeDisposers.splice(0)) {
    dispose();
  }
  jest.clearAllTimers();
  jest.useRealTimers();
});

async function loadPlugin() {
  importCounter += 1;
  const module = await import(`./herdr-tui-session.js?test=${importCounter}`);
  return module.default;
}

function fakeApi() {
  const sessions = new Map<string, { id: string; parentID?: string }>();
  const statuses: Record<string, { type: string }> = {};
  const permissions: Array<{ id: string; sessionID: string; tool?: { messageID: string; callID: string } }> = [];
  const questions: typeof permissions = [];
  const messages = new Map<string, { info?: { error?: { name: string } }; parts: Array<object> }>();
  const listeners = new Map<string, Set<(event: object) => void>>();
  const calls: string[] = [];
  let current: { name: string; params?: { sessionID: string } } = { name: "home" };
  let dispose: (() => void) | undefined;
  activeDisposers.push(() => dispose?.());

  return {
    statuses, permissions, questions, messages, listeners, calls,
    emit(type: string, properties: object) {
      for (const receive of listeners.get(type) ?? []) receive({ type, properties });
    },
    api: {
      client: {
        session: {
          async get({ sessionID }: { sessionID: string }) {
            calls.push(`get:${sessionID}`);
            const data = sessions.get(sessionID);
            if (!data) throw new Error("session not found");
            return { data };
          },
          async status() { calls.push("status"); return { data: { ...statuses } }; },
          async message({ messageID }: { sessionID: string; messageID: string }) {
            calls.push(`message:${messageID}`);
            const data = messages.get(messageID);
            if (!data) throw new Error("message unavailable");
            return { data };
          },
        },
        permission: { async list() { return { data: [...permissions] }; } },
        question: { async list() { return { data: [...questions] }; } },
      },
      event: {
        on(type: string, receive: (event: object) => void) {
          if (!listeners.has(type)) listeners.set(type, new Set());
          listeners.get(type)!.add(receive);
          return () => listeners.get(type)!.delete(receive);
        },
      },
      route: {
        get current() {
          return current;
        },
      },
      state: {
        session: {
          get(sessionID: string) {
            return sessions.get(sessionID);
          },
        },
      },
      lifecycle: {
        onDispose(handler: () => void) {
          dispose = handler;
          return () => {};
        },
      },
    },
    addSession(session: { id: string; parentID?: string }) {
      sessions.set(session.id, session);
    },
    select(sessionID: string) {
      current = { name: "session", params: { sessionID } };
    },
    home() { current = { name: "home" }; },
    dispose() {
      dispose?.();
    },
  };
}

// setImmediate stays real under fake timers and runs after every queued
// microtask, so the mocked socket's whole report chain has settled by then.
const drain = () => new Promise((resolve) => setImmediate(resolve));

// Why drain before advancing: the reports queued by the last step must reach
// the mocked socket before any timer that reads the plugin's state runs.
async function advance(ms: number) {
  await drain();
  jest.advanceTimersByTime(ms);
  await drain();
}

test("reports a root session when only the local route changes", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  await plugin.tui(tui.api);

  await advance(100);
  expect(requests).toHaveLength(0);

  tui.select("session-a");
  await advance(100);

  expect(requests.filter((r) => requestParam(r, "state") === undefined)).toHaveLength(1);
  expect(requestParam(requests[0], "agent_session_id")).toBe("session-a");
  expect(requestParam(requests[0], "session_start_source")).toBe("select");
  expect(requestParam(requests[0], "seq")).toBeUndefined();
});

test("retries an initial selection while Herdr detects the process", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.select("session-a");

  await plugin.tui(tui.api);
  await advance(125);

  const selections = requests.filter((request) => requestParam(request, "state") === undefined);
  expect(selections.length).toBeGreaterThanOrEqual(2);
  expect(selections.every((request) => requestParam(request, "agent_session_id") === "session-a")).toBe(true);
});

test("does not report root sessions not selected by this TUI", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.addSession({ id: "session-b" });
  tui.select("session-a");
  await plugin.tui(tui.api);

  await advance(125);

  expect(requests.length).toBeGreaterThan(0);
  expect(requests.every((request) => requestParam(request, "agent_session_id") === "session-a")).toBe(
    true,
  );
});

test("does not replace the root session with a selected child session", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "root-session" });
  tui.addSession({ id: "child-session", parentID: "root-session" });
  tui.select("root-session");
  await plugin.tui(tui.api);
  await drain();

  tui.select("child-session");
  await advance(125);

  expect(requests.length).toBeGreaterThan(0);
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "root-session")).toBe(true);
});

test("stops route polling when the TUI plugin is disposed", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  await plugin.tui(tui.api);
  tui.dispose();
  tui.select("session-a");

  await advance(125);

  expect(requests).toHaveLength(0);
});

function requestParam(request: unknown, name: string): unknown {
  if (!isRecord(request) || !isRecord(request.params)) {
    return undefined;
  }
  return request.params[name];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function v2Api() {
  const sessions = new Map([
    ["a", { id: "a" }],
    ["b", { id: "b" }],
    ["child", { id: "child", parentID: "a" }],
  ]);
  let route = { type: "session", sessionID: "a" };
  const listeners = new Set<(event: unknown) => void>();
  const permissions = new Map<string, Array<{ id: string }> | undefined>();
  const forms = new Map<string, Array<{ id: string }> | undefined>();
  return {
    api: {
      ui: { router: { current: () => route } },
      data: {
        session: {
          get: (id: string) => sessions.get(id),
          family: () => [...sessions.keys()],
          status: () => "idle",
          permission: { list: (id: string) => permissions.get(id) },
          form: { list: (id: string) => forms.get(id) },
        },
        listen: (handler: (event: unknown) => void) => {
          listeners.add(handler);
          return () => listeners.delete(handler);
        },
      },
    },
    select(sessionID: string) { route = { type: "session", sessionID }; },
    home() { route = { type: "home", sessionID: "" }; },
    emit(type: string, data?: object) {
      for (const listener of listeners) listener({ details: { type, data } });
    },
    listeners,
    sessions,
    permissions,
    forms,
  };
}

const states = () => requests.filter((r) => requestParam(r, "state") !== undefined)
  .map((r) => requestParam(r, "state"));

function familyApi() {
  const tui = fakeApi();
  tui.addSession({ id: "root" });
  tui.addSession({ id: "child", parentID: "root" });
  tui.addSession({ id: "sibling", parentID: "root" });
  tui.addSession({ id: "grandchild", parentID: "child" });
  tui.addSession({ id: "other" });
  tui.select("root");
  return tui;
}

test("V1 hydrates active descendants and keeps working until the whole family settles", async () => {
  const tui = familyApi();
  tui.statuses.child = { type: "busy" };
  tui.statuses.grandchild = { type: "retry" };
  tui.statuses.other = { type: "busy" };
  await (await loadPlugin()).tui(tui.api);
  await drain();
  expect(states().at(-1)).toBe("working");
  tui.emit("session.status", { sessionID: "child", status: { type: "idle" } });
  await drain();
  expect(states().at(-1)).toBe("working");
  tui.emit("session.status", { sessionID: "grandchild", status: { type: "idle" } });
  await drain();
  expect(states().at(-1)).toBe("idle");
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "root")).toBe(true);
});

test("V1 retains sibling blockers and cancellation does not finish an active parent", async () => {
  const tui = familyApi();
  tui.statuses.root = { type: "busy" };
  await (await loadPlugin()).tui(tui.api);
  await drain();
  tui.emit("permission.asked", { id: "p", sessionID: "child" });
  tui.emit("question.asked", { id: "q", sessionID: "sibling", tool: { messageID: "m", callID: "call" } });
  await drain();
  expect(states().at(-1)).toBe("blocked");
  tui.emit("permission.replied", { requestID: "p", sessionID: "child" });
  await drain();
  expect(states().at(-1)).toBe("blocked");
  tui.emit("session.idle", { sessionID: "sibling" });
  await drain();
  expect(states().at(-1)).toBe("blocked");
  tui.emit("message.part.updated", { part: {
    type: "tool", sessionID: "sibling", messageID: "m", callID: "call", state: { status: "error" },
  } });
  await drain();
  expect(states().at(-1)).toBe("working");
  tui.emit("session.idle", { sessionID: "root" });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V1 hydration rejects aborted tool requests even if their session is busy again", async () => {
  for (const kind of ["permissions", "questions"] as const) {
    const tui = familyApi();
    tui.statuses.child = { type: "busy" };
    tui[kind].push({ id: "stale", sessionID: "child", tool: { messageID: "m", callID: "call" } });
    tui.messages.set("m", { info: { error: { name: "MessageAbortedError" } }, parts: [
      { type: "tool", callID: "call", state: { status: "error" } },
    ] });
    await (await loadPlugin()).tui(tui.api);
    await drain();
    expect(states().at(-1)).toBe("working");
    tui.dispose();
  }
});

test("V1 validates pending tools instead of assuming an idle owner has no requests", async () => {
  const tui = familyApi();
  tui.permissions.push({ id: "pending", sessionID: "child", tool: { messageID: "m", callID: "call" } });
  tui.messages.set("m", { parts: [{ type: "tool", callID: "call", state: { status: "running" } }] });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  expect(states().at(-1)).toBe("blocked");
  tui.emit("message.part.updated", { part: {
    type: "tool", sessionID: "child", messageID: "m", callID: "call", state: { status: "error" },
  } });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V1 replays completion and replies over a late initial snapshot", async () => {
  const tui = familyApi();
  let resolveStatus!: (result: { data: Record<string, { type: string }> }) => void;
  tui.api.client.session.status = () => new Promise((resolve) => { resolveStatus = resolve; });
  tui.permissions.push({ id: "p", sessionID: "child" });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  expect(states()).not.toContain("idle");
  tui.emit("session.idle", { sessionID: "child" });
  tui.emit("permission.replied", { sessionID: "child", requestID: "p" });
  resolveStatus({ data: { child: { type: "busy" } } });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V1 retains replies received between failed hydration and its retry", async () => {
  const tui = familyApi();
  tui.permissions.push({ id: "p", sessionID: "child" });
  const status = tui.api.client.session.status;
  tui.api.client.session.status = async () => {
    tui.api.client.session.status = status;
    throw new Error("temporarily unavailable");
  };
  await (await loadPlugin()).tui(tui.api);
  await drain();
  tui.emit("permission.replied", { sessionID: "child", requestID: "p" });
  await advance(650);
  expect(states().at(-1)).toBe("idle");
  expect(states()).not.toContain("blocked");
});

test("V1 deleted sessions cannot return through a late hydration snapshot", async () => {
  const tui = familyApi();
  let resolveStatus!: (result: { data: Record<string, { type: string }> }) => void;
  tui.api.client.session.status = () => new Promise((resolve) => { resolveStatus = resolve; });
  tui.permissions.push({ id: "p", sessionID: "child" });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  tui.emit("session.deleted", { info: { id: "child", parentID: "root" } });
  resolveStatus({ data: { child: { type: "busy" } } });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V1 ignores stale hydration across A/B/A route changes and disposal", async () => {
  const tui = familyApi();
  let resolveOld!: (result: { data: Record<string, { type: string }> }) => void;
  const status = tui.api.client.session.status;
  tui.api.client.session.status = () => {
    tui.api.client.session.status = status;
    return new Promise((resolve) => { resolveOld = resolve; });
  };
  await (await loadPlugin()).tui(tui.api);
  tui.select("other");
  tui.emit("session.updated", { info: { id: "other" } });
  await drain();
  tui.select("root");
  tui.emit("session.updated", { info: { id: "root" } });
  await drain();
  requests.length = 0;
  resolveOld({ data: { root: { type: "busy" } } });
  await drain();
  expect(states()).not.toContain("working");
  tui.dispose();
  expect([...tui.listeners.values()].every((set) => set.size === 0)).toBe(true);
});

test("V1 a directly attached child owns its descendants, not its parent or siblings", async () => {
  const tui = familyApi();
  tui.select("child");
  tui.statuses.root = { type: "busy" };
  tui.statuses.sibling = { type: "busy" };
  await (await loadPlugin()).tui(tui.api);
  await drain();
  expect(states().at(-1)).toBe("idle");
  tui.emit("session.status", { sessionID: "grandchild", status: { type: "busy" } });
  await drain();
  expect(states().at(-1)).toBe("working");
  tui.select("grandchild");
  tui.emit("session.updated", { info: { id: "grandchild", parentID: "child" } });
  await drain();
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "child")).toBe(true);
});

test("V1 a terminal tool event during hydration cannot revive a cancelled blocker", async () => {
  const tui = familyApi();
  tui.questions.push({ id: "q", sessionID: "child", tool: { messageID: "m", callID: "call" } });
  let resolveMessage!: (result: { data: { parts: Array<object> } }) => void;
  tui.api.client.session.message = () => new Promise((resolve) => { resolveMessage = resolve; });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  tui.emit("message.part.updated", { part: {
    type: "tool", sessionID: "child", messageID: "m", callID: "call", state: { status: "error" },
  } });
  resolveMessage({ data: { parts: [{ type: "tool", callID: "call", state: { status: "running" } }] } });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V1 unknown tool evidence remains blocked and retries failed message reads", async () => {
  const tui = familyApi();
  tui.permissions.push({ id: "p", sessionID: "child", tool: { messageID: "m", callID: "call" } });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  expect(states().at(-1)).toBe("blocked");
  tui.messages.set("m", { parts: [{ type: "tool", callID: "call", state: { status: "error" } }] });
  await advance(650);
  expect(states().at(-1)).toBe("idle");
});

test("V1 request validation matches the exact tool and deduplicates message reads", async () => {
  const tui = familyApi();
  tui.permissions.push({ id: "p", sessionID: "child", tool: { messageID: "m", callID: "running" } });
  tui.questions.push({ id: "q", sessionID: "child", tool: { messageID: "m", callID: "finished" } });
  tui.messages.set("m", { parts: [
    { type: "tool", callID: "running", state: { status: "running" } },
    { type: "tool", callID: "finished", state: { status: "completed" } },
  ] });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  expect(states().at(-1)).toBe("blocked");
  expect(tui.calls.filter((call) => call === "message:m")).toHaveLength(1);
  tui.emit("permission.replied", { sessionID: "child", requestID: "p" });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V1 reselecting an aborted request does not resurrect it or poll completed tools", async () => {
  const tui = familyApi();
  tui.permissions.push({ id: "p", sessionID: "child", tool: { messageID: "m", callID: "call" } });
  tui.messages.set("m", { parts: [{ type: "tool", callID: "call", state: { status: "error" } }] });
  await (await loadPlugin()).tui(tui.api);
  await drain();
  tui.select("other");
  tui.emit("session.updated", { info: { id: "other" } });
  await drain();
  tui.select("root");
  tui.emit("session.updated", { info: { id: "root" } });
  await drain();
  expect(states()).not.toContain("blocked");
  const reads = tui.calls.length;
  await advance(650);
  expect(tui.calls).toHaveLength(reads);
});

test("V1 home and selected-session deletion settle authority and retry a dropped idle report", async () => {
  for (const action of ["home", "delete"]) {
    const tui = familyApi();
    tui.statuses.root = { type: "busy" };
    await (await loadPlugin()).tui(tui.api);
    await drain();
    expect(states().at(-1)).toBe("working");
    failConnections = true;
    if (action === "home") {
      tui.home();
      tui.emit("session.updated", { info: { id: "root" } });
    } else tui.emit("session.deleted", { info: { id: "root" } });
    await drain();
    failConnections = false;
    expect(states().at(-1)).toBe("working");
    await advance(400);
    expect(states().at(-1)).toBe("working");
    await advance(200);
    expect(states().at(-1)).toBe("idle");
    tui.dispose();
  }
});

test("V1 a delayed home settlement cannot overwrite the next selected session", async () => {
  const tui = familyApi();
  tui.statuses.root = { type: "busy" };
  await (await loadPlugin()).tui(tui.api);
  await drain();
  requests.length = 0;
  holdConnections = true;
  tui.home();
  tui.emit("session.updated", { info: { id: "root" } });
  await drain();
  expect(connections.length).toBeGreaterThan(0);
  tui.select("other");
  tui.emit("session.updated", { info: { id: "other" } });
  holdConnections = false;
  for (const connect of connections.splice(0)) connect();
  await drain();
  expect(requests.length).toBeGreaterThan(0);
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "other")).toBe(true);
  expect(states().at(-1)).toBe("idle");
});

test("V2 ignores events without data", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  requests.length = 0;
  expect(() => tui.emit("legacy.event")).not.toThrow();
  tui.emit("session.execution.started", { sessionID: "a" });
  await drain();
  expect(states()).toEqual(["working"]);
});

test("V2 an overdue selection retry handled with an event still reports the event's state last and in order", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  requests.length = 0;
  // A stalled runner: the retry deadline passes before the poll can run.
  jest.setSystemTime(Date.now() + 150);
  tui.emit("session.execution.started", { sessionID: "a" });
  await drain();
  expect(states().at(-1)).toBe("working");
  const seqs = requests.map((r) => requestParam(r, "seq")).filter((seq) => seq !== undefined) as number[];
  expect(seqs).toEqual([...seqs].sort((a, b) => a - b));
  expect(new Set(seqs).size).toBe(seqs.length);
});

test("V2 completes and interrupts without legacy idle events", async () => {
  for (const terminal of ["succeeded", "interrupted", "failed"]) {
    const plugin = await loadPlugin();
    const tui = v2Api();
    const dispose = await plugin.setup(tui.api);
    activeDisposers.push(dispose);
    await drain();
    requests.length = 0;
    tui.emit("session.execution.started", { sessionID: "a" });
    tui.emit(`session.execution.${terminal}`, { sessionID: "a" });
    await drain();
    expect(states()).toEqual(["working", terminal === "failed" ? "blocked" : "idle"]);
    dispose();
  }
});

test("V2 aggregates root and child blockers and ignores other roots and child completion", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  requests.length = 0;
  tui.emit("session.execution.started", { sessionID: "a" });
  tui.emit("permission.asked", { sessionID: "a", id: "permission-a" });
  tui.emit("form.created", { form: { sessionID: "child", id: "form-child" } });
  tui.emit("permission.replied", { sessionID: "a", requestID: "permission-a" });
  tui.emit("session.execution.succeeded", { sessionID: "child" });
  tui.emit("session.execution.started", { sessionID: "b" });
  tui.emit("permission.asked", { sessionID: "b", id: "other" });
  await drain();
  expect(states().at(-1)).toBe("blocked");
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "a")).toBe(true);
  tui.emit("form.cancelled", { sessionID: "child", id: "form-child" });
  tui.emit("session.execution.succeeded", { sessionID: "a" });
  await drain();
  expect(states().slice(-2)).toEqual(["working", "idle"]);
});

test("V2 discards queued reports after selection changes and stops on disposal", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  requests.length = 0;
  tui.emit("session.execution.started", { sessionID: "a" });
  tui.select("b");
  tui.emit("session.execution.started", { sessionID: "b" });
  await drain();
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "b")).toBe(true);
  requests.length = 0;
  tui.emit("session.execution.succeeded", { sessionID: "b" });
  tui.home();
  await drain();
  expect(requests).toHaveLength(0);
  dispose();
  expect(tui.listeners.size).toBe(0);
  tui.select("a");
  await advance(250);
  expect(requests).toHaveLength(0);
});

test("V2 reconciles late blocker hydration without reviving an already-replied request", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  requests.length = 0;
  tui.permissions.set("child", [{ id: "late" }]);
  await advance(100);
  expect(states()).toEqual(["blocked"]);
  tui.emit("permission.replied", { sessionID: "child", requestID: "late" });
  await drain();
  expect(states()).toEqual(["blocked", "idle"]);
  // The cache still lists the replied request until it catches up.
  await advance(100);
  expect(states()).toEqual(["blocked", "idle"]);
  tui.permissions.set("child", []);
  tui.forms.set("child", [{ id: "second" }]);
  await advance(100);
  expect(states()).toEqual(["blocked", "idle", "blocked"]);
  tui.sessions.delete("child");
  tui.emit("session.deleted", { sessionID: "child" });
  await drain();
  expect(states().at(-1)).toBe("idle");
});

test("V2 never writes a delayed connection after disposal or a session switch", async () => {
  for (const action of ["dispose", "switch"]) {
    const plugin = await loadPlugin();
    const tui = v2Api();
    holdConnections = true;
    requests.length = 0;
    const dispose = await plugin.setup(tui.api);
    activeDisposers.push(dispose);
    await drain();
    expect(connections.length).toBeGreaterThan(0);
    if (action === "dispose") dispose();
    else tui.select("b");
    holdConnections = false;
    for (const connect of connections.splice(0)) connect();
    await drain();
    expect(requests).toHaveLength(0);
    dispose();
  }
});

test("V2 settles a connection that never completes", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  holdConnections = true;
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  // The report queue is serial, so it only opens another connection once the
  // stuck one's connect timeout has settled it.
  await advance(1_600);
  const stalled = connections.length;
  await advance(600);
  expect(connections.length).toBeGreaterThan(stalled);
  dispose();
});

test("V2 resends the latest state after a failed delivery", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await drain();
  // Exhaust the selection retry schedule so only the event report remains.
  await advance(1_600);
  requests.length = 0;
  tui.emit("session.execution.started", { sessionID: "a" });
  await drain();
  failConnections = true;
  tui.emit("session.execution.succeeded", { sessionID: "a" });
  await drain();
  expect(states()).toEqual(["working"]);
  failConnections = false;
  await advance(499);
  expect(states()).toEqual(["working"]);
  await advance(1);
  expect(states()).toEqual(["working", "idle"]);
  dispose();
});
