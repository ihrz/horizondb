import { describe, expect, test } from "bun:test";
import {
  DEFAULT_TABLE,
  HorizonDBAuthError,
  HorizonDBOpError,
  HorizonDBTimeoutError,
} from "../src";
import { InvalidTableNameError, isValidTableName, requireKey, resolveTable } from "../src/utils";

describe("table name validation", () => {
  test("accepts sql-safe identifiers", () => {
    for (const name of ["json", "_users", "Table9", "a", "snake_case_42"]) {
      expect(isValidTableName(name)).toBe(true);
    }
  });

  test("rejects injection attempts and malformed names", () => {
    const longName = "a".repeat(64);
    for (const name of [
      "",
      "9start",
      "-dash",
      "with space",
      "with.dot",
      'quote"',
      "semicolon;",
      "drop;select--",
      longName,
    ]) {
      expect(isValidTableName(name)).toBe(false);
    }
  });

  test("resolveTable falls back to the default table", () => {
    expect(resolveTable()).toBe(DEFAULT_TABLE);
    expect(resolveTable("custom")).toBe("custom");
    expect(resolveTable(undefined)).toBe(DEFAULT_TABLE);
  });

  test("resolveTable throws a typed error for hostile names", () => {
    try {
      resolveTable('json"; DROP TABLE users; --');
      throw new Error("should have thrown");
    } catch (error) {
      expect(error).toBeInstanceOf(InvalidTableNameError);
      expect(error).toBeInstanceOf(TypeError);
    }
  });
});

describe("key validation", () => {
  test("requireKey accepts normal keys", () => {
    expect(requireKey("user.name")).toBe("user.name");
    expect(requireKey("counter")).toBe("counter");
  });

  test("requireKey rejects empty keys", () => {
    expect(() => requireKey("")).toThrow(TypeError);
    expect(() => requireKey(undefined)).toThrow(TypeError);
  });
});

describe("error types keep instanceof chains", () => {
  test("op errors are TypeErrors", () => {
    expect(new HorizonDBOpError("boom")).toBeInstanceOf(TypeError);
  });

  test("timeout errors carry the operation", () => {
    const error = new HorizonDBTimeoutError("get");
    expect(error.message).toContain("get");
    expect(error.name).toBe("HorizonDBTimeoutError");
  });

  test("auth errors are distinct", () => {
    expect(new HorizonDBAuthError().name).toBe("HorizonDBAuthError");
  });
});
