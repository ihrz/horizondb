import { beforeAll, afterAll, beforeEach, afterEach, describe, expect, test } from "bun:test";
import { HorizonDB } from "../src/client";
import { ConnectionClosedError, HorizonDBAuthError, HorizonDBTimeoutError } from "../src/connection";
import type { Json } from "../src/types";
import { MockHorizonServer } from "./helpers/mock-server";

const CREDENTIALS = {
  username: "admin",
  password: "secret",
} as const;

describe("HorizonDB", () => {
  let server: MockHorizonServer;
  let db: HorizonDB;

  beforeAll(() => {
    server = new MockHorizonServer(CREDENTIALS);
  });

  afterAll(() => {
    server.stop();
  });

  beforeEach(async () => {
    db = await HorizonDB.connect({
      url: server.url,
      ...CREDENTIALS,
      requestTimeoutMs: 2_000,
    });
    await db.deleteAll();
    await db.table("otherTable").deleteAll();
  });

  afterEach(() => {
    db.close();
  });

  test("should set and get a value", async () => {
    await db.set("user.name", "Alice");
    expect(await db.get("user.name")).toBe("Alice");

    await db.set("test", {
      message: "Hello, World!",
      number: 42,
      boolean: true,
      array: [1, 2, 3],
      object: { key: "value" },
      nullable: null,
    } as unknown as Json);

    expect(await db.get<Json[]>("test.array")).toEqual([1, 2, 3]);
    expect((await db.get<{ number: number }>("test"))?.number).toBe(42);
  });

  test("should return undefined for missing keys and honor defaultValue", async () => {
    expect(await db.get("user.nonexistent")).toBeUndefined();
    expect(await db.get("user.nonexistent", "fallback")).toBe("fallback");
    expect(await db.get("user.nonexistent", 7)).toBe(7);
  });

  test("should add a value to a nested key", async () => {
    await db.set("user.age", 30);
    await db.add("user.age", 5);
    expect(await db.get("user.age")).toBe(35);
  });

  test("should subtract a value from a nested key", async () => {
    await db.set("user.age", 30);
    await db.sub("user.age", 5);
    expect(await db.get("user.age")).toBe(25);
  });

  test("should delete a nested key", async () => {
    await db.set("user.name", "Alice");
    await db.delete("user.name");
    expect(await db.get("user.name")).toBeUndefined();
  });

  test("should cache a value and delete it after the specified time", async () => {
    await db.cache("session.token", "abc123", 300);
    expect(await db.get("session.token")).toBe("abc123");

    await Bun.sleep(500);
    expect(await db.get("session.token")).toBeUndefined();
  });

  test("should add to a non-existing key", async () => {
    await db.add("counter", 5);
    expect(await db.get("counter")).toBe(5);
  });

  test("should subtract from a non-existing key", async () => {
    await db.sub("counter", 5);
    expect(await db.get("counter")).toBe(-5);
  });

  test("should push an element to an array", async () => {
    await db.push("tasks", "task1");
    await db.push("tasks", "task2");
    expect(await db.get("tasks")).toEqual(["task1", "task2"]);
  });

  test("should pull elements from an array", async () => {
    await db.set("array1", [1, 2, 3]);
    expect(await db.get("array1")).toEqual([1, 2, 3]);

    await db.pull("array1", 10);
    expect(await db.get("array1")).toEqual([1, 2, 3]);

    await db.pull("array1", 2);
    expect(await db.get("array1")).toEqual([1, 3]);

    await db.pull("array1", 1);
    expect(await db.get("array1")).toEqual([3]);

    await db.delete("array1");
    expect(await db.get("array1")).toBeUndefined();
  });

  test("should return all key-value pairs", async () => {
    await db.set("user.name", "Alice");
    await db.set("user.age", 30);
    const all = await db.all();
    expect(all).toEqual([
      {
        id: "user",
        value: { name: "Alice", age: 30 },
      },
    ]);
  });

  test("should delete all data in the current table", async () => {
    await db.set("user.name", "Alice");
    await db.deleteAll();
    expect(await db.get("user.name")).toBeUndefined();
    expect(await db.all()).toEqual([]);
  });

  test("should switch between tables", async () => {
    await db.set("user.name", "Alice");
    await db.table("otherTable").set("user.name", "Bob");

    expect(await db.get("user.name")).toBe("Alice");
    expect(await db.table("otherTable").get("user.name")).toBe("Bob");

    const other = db.table("otherTable");
    expect(await other.has("user.name")).toBe(true);
    expect(await db.has("user.name")).toBe(true);
  });

  test("should handle missing keys gracefully", async () => {
    expect(await db.get("user.nonexistent")).toBeUndefined();
    await db.add("user.nonexistent.age", 5);
    expect(await db.get("user.nonexistent.age")).toBe(5);
  });

  test("should throw an error for invalid operations", async () => {
    await db.set("user.name", "Alice");
    await expect(db.add("user.name", 5)).rejects.toThrow(TypeError);
    await expect(db.sub("user.name", 5)).rejects.toThrow(TypeError);
  });

  test("should handle deeply nested properties", async () => {
    await db.set("level1.level2.level3", "deepValue");
    expect(await db.get("level1.level2.level3")).toBe("deepValue");

    await db.add("deep.a.b.count", 10);
    expect(await db.get("deep.a.b.count")).toBe(10);

    await db.sub("deep.a.b.count", 2);
    expect(await db.get("deep.a.b.count")).toBe(8);

    await db.delete("deep.a.b.count");
    expect(await db.get("deep.a.b.count")).toBeUndefined();
  });

  test("should reject arithmetic through a scalar the way the server does", async () => {
    await db.set("lvl.leaf", "text");
    await expect(db.add("lvl.leaf.child", 1)).rejects.toThrow(TypeError);
    await expect(db.push("lvl.leaf.items", 1)).rejects.toThrow(TypeError);
  });

  test("should reject out-of-range ttl values", async () => {
    await expect(db.cache("ttl.zero", 1, 0)).rejects.toThrow(/ttl/i);
  });

  test("should pipeline concurrent operations without cross-talk", async () => {
    const writes = Array.from({ length: 50 }, (_, index) =>
      db.set(`parallel.${index}`, { index, even: index % 2 === 0 }),
    );
    await Promise.all(writes);

    const reads = Array.from({ length: 50 }, (_, index) => db.get(`parallel.${index}`));
    const values = await Promise.all(reads);

    values.forEach((value, index) => {
      expect(value).toEqual({ index, even: index % 2 === 0 });
    });
  });

  test("should reject unknown operations with the server message", async () => {
    await expect(
      (db as unknown as { connection: { request: Function } }).connection.request(
        "frobnicate",
      ),
    ).rejects.toThrow(/unknown operation/i);
  });

  test("should report the new number from add and sub", async () => {
    expect(await db.add("counter.hits", 5)).toBe(5);
    expect(await db.add("counter.hits")).toBe(6);
    expect(await db.sub("counter.hits", 2)).toBe(4);
    expect(await db.sub("bare")).toBe(-1);
  });

  test("should return arrays from push and pull only when asked", async () => {
    // Nested push on a document that does not exist yet creates the object.
    expect(await db.push("list.items", 1)).toBeUndefined();
    expect(await db.push("list.items", 2, { returnValue: true })).toEqual([1, 2]);
    expect(await db.pull("list.items", 1)).toBeUndefined();
    expect(await db.pull("list.items", 2, { returnValue: true })).toEqual([]);
  });

  test("should report whether a deleted key existed", async () => {
    await db.set("user.name", "Alice");
    expect(await db.delete("user.name")).toBe(true);
    expect(await db.delete("user.name")).toBe(false);
    expect(await db.delete("user")).toBe(false);
    await db.set("user.age", 3);
    expect(await db.delete("user")).toBe(true);
  });

  test("should transparently re-login after logout when credentials are stored", async () => {
    await db.logout();
    expect(await db.get("anything")).toBeUndefined();

    await db.set("after.relogin", true);
    expect(await db.get("after.relogin")).toBe(true);
  });
});

describe("HorizonDB connection handling", () => {
  let server: MockHorizonServer;

  beforeAll(() => {
    server = new MockHorizonServer(CREDENTIALS);
  });

  afterAll(() => {
    server.stop();
  });

  test("should reject bad credentials at connect time", async () => {
    await expect(
      HorizonDB.connect({
        url: server.url,
        username: "admin",
        password: "wrong",
        requestTimeoutMs: 1_000,
      }),
    ).rejects.toThrow(HorizonDBAuthError);
  });

  test("should reject operations when no credentials were provided", async () => {
    const anonymous = await HorizonDB.connect({ url: server.url });
    await expect(anonymous.get("key")).rejects.toThrow(/session/i);
    anonymous.close();
  });

  test("should time out when the server never answers", async () => {
    const blackhole = silentServer();
    try {
      const silent = new HorizonDB({
        url: `ws://127.0.0.1:${blackhole.port}`,
        username: "admin",
        password: "secret",
        requestTimeoutMs: 100,
      });
      await expect(silent.ready()).rejects.toThrow(HorizonDBTimeoutError);
      silent.close();
    } finally {
      blackhole.stop(true);
    }
  });

  test("should wait for the re-login before replaying requests after a restart", async () => {
    const restartable = new MockHorizonServer({ ...CREDENTIALS, port: 0 });
    const port = restartable.port;
    const client = await HorizonDB.connect({
      url: restartable.url,
      ...CREDENTIALS,
      requestTimeoutMs: 5_000,
    });
    try {
      await client.set("before", 1);
      expect(restartable.loginCount).toBe(1);

      // Kill the server: every socket is dropped, the SDK starts reconnecting.
      restartable.stop();
      await new Promise((resolve) => setTimeout(resolve, 50));
      const replacement = new MockHorizonServer({ ...CREDENTIALS, port });
      try {
        // Requests issued while the link is down must wait for open + login
        // and must not each trigger their own login.
        const results = await Promise.all(
          Array.from({ length: 8 }, (_, index) =>
            client.set(`after.${index}`, index).then(() => client.get(`after.${index}`)),
          ),
        );
        expect(results).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
        expect(replacement.loginCount).toBe(1);
        expect(client.connectionCount).toBe(2);
      } finally {
        replacement.stop();
      }
    } finally {
      client.close();
    }
  });

  test("should reset a connection whose server went silent and recover", async () => {
    const flaky = new MockHorizonServer(CREDENTIALS);
    const client = await HorizonDB.connect({
      url: flaky.url,
      ...CREDENTIALS,
      requestTimeoutMs: 150,
    });
    try {
      await client.set("alive", true);
      expect(flaky.openCount).toBe(1);

      flaky.silent = true;
      await expect(client.get("alive")).rejects.toThrow(HorizonDBTimeoutError);
      // The timeout with a completely silent link forces a fresh socket
      // (reconnect backoff starts at 250 ms, above this test's timeout).
      flaky.silent = false;
      await new Promise((resolve) => setTimeout(resolve, 600));
      expect(await client.get("alive")).toBe(true);
      expect(flaky.openCount).toBe(2);
      expect(client.connectionCount).toBe(2);
    } finally {
      client.close();
      flaky.stop();
    }
  });

  test("should not reset a slow but answering connection", async () => {
    const slow = new MockHorizonServer(CREDENTIALS);
    const client = await HorizonDB.connect({
      url: slow.url,
      ...CREDENTIALS,
      requestTimeoutMs: 150,
    });
    try {
      // One request is dropped while another one is answered within the
      // window: the server is slow, not dead, so the socket must stay.
      slow.silent = true;
      const dropped = client.get("x");
      await new Promise((resolve) => setTimeout(resolve, 50));
      slow.silent = false;
      await client.set("y", 1);
      await expect(dropped).rejects.toThrow(HorizonDBTimeoutError);
      expect(await client.get("y")).toBe(1);
      expect(slow.openCount).toBe(1);
    } finally {
      client.close();
      slow.stop();
    }
  });

  test("should reject pending requests when closed mid-flight", async () => {
    const blackhole = silentServer();
    try {
      const client = new HorizonDB({
        url: `ws://127.0.0.1:${blackhole.port}`,
        requestTimeoutMs: 5_000,
      });
      client.close();
      await expect(client.ready()).rejects.toBeInstanceOf(Error);
    } finally {
      blackhole.stop(true);
    }
    expect(new ConnectionClosedError().message.length).toBeGreaterThan(0);
  });
});

function silentServer(): Bun.Server<undefined> {
  return Bun.serve<undefined>({
    port: 0,
    fetch(request, server) {
      if (server.upgrade(request)) {
        return;
      }
      return new Response("websocket upgrade required", { status: 426 });
    },
    websocket: {
      message() {},
    },
  });
}
