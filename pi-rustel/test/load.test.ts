/** Load smoke for the pi entry: the module loads without a pi installation
 * present (its pi import is type-only), registers the four tools and the
 * shutdown handler, and the tools answer through a stub ExtensionAPI. The
 * tools' behaviour against a real rustel child is covered by
 * integration.test.ts. */

import assert from "node:assert/strict";
import test from "node:test";
import factory from "../src/index.ts";

type RegisteredTool = {
  name: string;
  parameters: Record<string, unknown>;
  execute: (
    toolCallId: string,
    params: Record<string, unknown>,
    signal: unknown,
    onUpdate: unknown,
    ctx: unknown,
  ) => Promise<unknown>;
};

type RegisteredHandler = (event: unknown, ctx: unknown) => unknown;

/** A minimal ExtensionAPI: enough surface for the extension to register
 * everything it offers, and to observe what it pushes. */
type StubPi = {
  tools: Map<string, RegisteredTool>;
  handlers: Map<string, RegisteredHandler>;
  messages: Array<{ customType: string; content: string }>;
  registerTool: (definition: RegisteredTool) => void;
  on: (name: string, handler: RegisteredHandler) => void;
  sendMessage: (message: { customType: string; content: string }) => void;
};

function stubPi(): StubPi {
  const stub: StubPi = {
    tools: new Map(),
    handlers: new Map(),
    messages: [],
    registerTool: (definition) => stub.tools.set(definition.name, definition),
    on: (name, handler) => stub.handlers.set(name, handler),
    sendMessage: (message) =>
      stub.messages.push({ customType: message.customType, content: message.content }),
  };
  return stub;
}

test("the entry registers the four tools and the shutdown handler", async () => {
  const pi = stubPi();
  await factory(pi as unknown as never);
  assert.deepEqual(
    [...pi.tools.keys()].sort(),
    ["rustel_start", "rustel_status", "rustel_stop", "rustel_update"],
  );
  assert.ok(pi.handlers.has("session_shutdown"), "session_shutdown must be handled");
  for (const tool of pi.tools.values()) {
    const schema = tool.parameters as { type?: string; required?: string[] };
    assert.equal(schema.type, "object", `${tool.name} takes an object`);
    assert.ok(
      schema.required === undefined || Array.isArray(schema.required),
      `${tool.name} declares its required arguments`,
    );
  }
  const start = pi.tools.get("rustel_start").parameters as { required?: string[] };
  const update = pi.tools.get("rustel_update").parameters as { required?: string[] };
  assert.deepEqual(start.required, ["set"]);
  assert.deepEqual(update.required, ["source"]);
});

test("the status tool answers when no set was started", async () => {
  const pi = stubPi();
  await factory(pi as unknown as never);
  const status = pi.tools.get("rustel_status");
  assert.ok(status !== undefined);
  const result = (await status.execute("id", {}, undefined, undefined, {
    cwd: process.cwd(),
  })) as { content: Array<{ type: string; text: string }>; isError?: boolean };
  assert.equal(result.isError, undefined);
  assert.match(result.content[0].text, /no set/);
});

test("a start on a set folder that does not exist fails honestly", async () => {
  const pi = stubPi();
  await factory(pi as unknown as never);
  const start = pi.tools.get("rustel_start");
  assert.ok(start !== undefined);
  const refused = (await start.execute(
    "id",
    { set: "/nonexistent/set" },
    undefined,
    undefined,
    { cwd: process.cwd() },
  )) as { isError?: boolean; content: Array<{ type: string; text: string }> };
  assert.equal(refused.isError, true);
  assert.match(refused.content[0].text, /does not exist/);
  assert.equal(pi.messages.length, 0, "a failed start pushes no session message");
});

test("update and stop say so when no set runs", async () => {
  const pi = stubPi();
  await factory(pi as unknown as never);
  const update = pi.tools.get("rustel_update");
  const stop = pi.tools.get("rustel_stop");
  assert.ok(update !== undefined && stop !== undefined);
  const refused = (await update.execute("id", { source: 's("bd")\n' }, undefined, undefined, {
    cwd: process.cwd(),
  })) as { isError?: boolean; content: Array<{ type: string; text: string }> };
  assert.equal(refused.isError, true);
  assert.match(refused.content[0].text, /no set is running/);
  const stopped = (await stop.execute("id", {}, undefined, undefined, {
    cwd: process.cwd(),
  })) as { isError?: boolean; content: Array<{ type: string; text: string }> };
  assert.equal(stopped.isError, undefined);
  assert.match(stopped.content[0].text, /no set was running/);
});
