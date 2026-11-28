import { describe, expect, test } from "bun:test";
import {
	assertKiroOptIn,
	isKiroOptedIn,
	KIRO_OPT_IN_ENV,
	KIRO_OPT_IN_NOTICE,
	KiroNotEnabledError,
} from "../src/wire/kiro-optin";
import { kiroCliOidcHeaders, kiroCliServiceHeaders } from "../src/wire/kiro-cli";
import { fetchKiroModels } from "../src/discovery/kiro";
import type { KiroCredential } from "../src/wire/kiro";

const OFF = {};
const ON = { [KIRO_OPT_IN_ENV]: "1" };

describe("Kiro opt-in", () => {
	test("is off by default", () => {
		expect(isKiroOptedIn(OFF)).toBe(false);
	});

	test.each(["1", "true", "TRUE", "yes", "Yes"])("%s opts in", value => {
		expect(isKiroOptedIn({ [KIRO_OPT_IN_ENV]: value })).toBe(true);
	});

	test.each(["", "0", "false", "no", "on", "2", " 1", "enabled"])("%j does not opt in", value => {
		expect(isKiroOptedIn({ [KIRO_OPT_IN_ENV]: value })).toBe(false);
	});

	test("the refusal names the variable and quotes the provider's own statement", () => {
		let error: unknown;
		try {
			assertKiroOptIn(OFF);
		} catch (caught) {
			error = caught;
		}
		expect(error).toBeInstanceOf(KiroNotEnabledError);
		const message = (error as Error).message;
		expect(message).toContain(KIRO_OPT_IN_ENV);
		expect(message).toContain("https://kiro.dev/faq");
		expect(message).toContain("not permitted");
		expect(message).toContain("kiro-cli acp");
	});

	test("does not throw once opted in", () => {
		expect(() => assertKiroOptIn(ON)).not.toThrow();
	});

	test("the notice for an opted-in user states the risk and the documented route", () => {
		expect(KIRO_OPT_IN_NOTICE).toContain("https://kiro.dev/faq");
		expect(KIRO_OPT_IN_NOTICE).toContain("official Kiro CLI");
		expect(KIRO_OPT_IN_NOTICE).toContain("kiro-cli acp");
	});
});

describe("Kiro requests ask for the opt-in first", () => {
	const attempt = { attempt: 1, maxAttempts: 1 };

	test("service headers are refused without it", () => {
		const previous = process.env[KIRO_OPT_IN_ENV];
		delete process.env[KIRO_OPT_IN_ENV];
		try {
			expect(() => kiroCliServiceHeaders("ListAvailableModels", "token", attempt)).toThrow(KiroNotEnabledError);
			expect(() => kiroCliOidcHeaders(attempt)).toThrow(KiroNotEnabledError);
		} finally {
			if (previous !== undefined) process.env[KIRO_OPT_IN_ENV] = previous;
		}
	});

	test("service headers are built once opted in", () => {
		const previous = process.env[KIRO_OPT_IN_ENV];
		process.env[KIRO_OPT_IN_ENV] = "1";
		try {
			const headers = kiroCliServiceHeaders("ListAvailableModels", "token", attempt);
			expect(headers.authorization).toBe("Bearer token");
			expect(Object.keys(kiroCliOidcHeaders(attempt))).toContain("user-agent");
		} finally {
			if (previous === undefined) delete process.env[KIRO_OPT_IN_ENV];
			else process.env[KIRO_OPT_IN_ENV] = previous;
		}
	});
});

describe("Kiro model discovery without the opt-in", () => {
	const credential: KiroCredential = { kind: "oauth", token: "access", region: "us-east-1", loginId: "login-a" };

	test("sends no request and returns null, so the cached roster is kept", async () => {
		const previous = process.env[KIRO_OPT_IN_ENV];
		delete process.env[KIRO_OPT_IN_ENV];
		const requests: string[] = [];
		try {
			const models = await fetchKiroModels({
				credential,
				fetch: async input => {
					requests.push(String(input));
					return Response.json({ models: [] });
				},
			});
			expect(models).toBeNull();
			expect(requests).toEqual([]);
		} finally {
			if (previous !== undefined) process.env[KIRO_OPT_IN_ENV] = previous;
		}
	});
});
