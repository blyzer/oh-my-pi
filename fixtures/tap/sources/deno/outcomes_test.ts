Deno.test("passes", () => {});

Deno.test("fails an assertion", () => {
  if (1 + 1 !== 3) {
    throw new Error("expected 3");
  }
});

Deno.test({ name: "skipped", ignore: true, fn() {} });

Deno.test("steps", async (t) => {
  await t.step("step passes", () => {});
  await t.step("step fails", () => {
    throw new Error("step failure");
  });
});
