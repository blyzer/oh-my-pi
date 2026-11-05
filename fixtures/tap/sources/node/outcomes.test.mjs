import assert from "node:assert/strict";
import { before, describe, it, test } from "node:test";

test("passes", () => {
  assert.equal(1, 1);
});

test("fails an assertion", () => {
  assert.equal(1 + 1, 3);
});

test("throws an error", () => {
  throw new Error("boom");
});

test("skipped", { skip: "not on this platform" }, () => {});

test("todo fails", { todo: "not implemented yet" }, () => {
  assert.fail("todo failure");
});

describe("suite", () => {
  it("nested passes", () => {});
  it("nested fails", () => {
    assert.deepEqual({ a: 1 }, { a: 2 });
  });
});

describe("setup", () => {
  before(() => {
    throw new Error("setup failed");
  });
  it("never runs", () => {});
});
