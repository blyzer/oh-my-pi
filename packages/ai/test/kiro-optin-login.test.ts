import { afterEach, beforeEach, describe, expect, it, spyOn } from "bun:test";
import { loginKiroHook, refreshKiroHook } from "@oh-my-pi/pi-ai/registry/oauth/kiro";
import type { OAuthController, OAuthCredentials, OAuthPrompt } from "@oh-my-pi/pi-ai/registry/oauth/types";
import type { FetchImpl } from "@oh-my-pi/pi-catalog/types";
import { KIRO_OPT_IN_ENV, KIRO_OPT_IN_NOTICE, KiroNotEnabledError } from "@oh-my-pi/pi-catalog/wire/kiro-optin";

let saved: string | undefined;

beforeEach(() => {
	saved = process.env[KIRO_OPT_IN_ENV];
	delete process.env[KIRO_OPT_IN_ENV];
});

afterEach(() => {
	if (saved === undefined) delete process.env[KIRO_OPT_IN_ENV];
	else process.env[KIRO_OPT_IN_ENV] = saved;
});

function controller(...answers: string[]) {
	const prompts: OAuthPrompt[] = [];
	const requests: string[] = [];
	const fetch: FetchImpl = async input => {
		requests.push(String(input));
		return new Response("{}");
	};
	const ctrl: OAuthController = {
		fetch,
		onAuth() {},
		onPrompt: async prompt => {
			prompts.push(prompt);
			return answers.shift() ?? Promise.reject(new Error(`unexpected prompt: ${prompt.message}`));
		},
	};
	return { ctrl, prompts, requests };
}

describe("Kiro login and refresh ask for the opt-in", () => {
	it("refuses to start a login, asks nothing and sends nothing", async () => {
		const { ctrl, prompts, requests } = controller("1");
		await expect(loginKiroHook(ctrl)).rejects.toThrow(KiroNotEnabledError);
		expect(prompts).toEqual([]);
		expect(requests).toEqual([]);
	});

	it("shows the notice before the login method prompt once opted in", async () => {
		process.env[KIRO_OPT_IN_ENV] = "1";
		const { ctrl, prompts } = controller("not-a-method");
		await expect(loginKiroHook(ctrl)).rejects.toThrow("Invalid Kiro authentication selection");
		expect(prompts).toHaveLength(1);
		expect(prompts[0]!.message.startsWith(KIRO_OPT_IN_NOTICE)).toBe(true);
	});

	it("refuses to refresh a social login and sends no request", async () => {
		const fetchSpy = spyOn(globalThis, "fetch").mockImplementation(
			(async () => new Response("{}")) as unknown as typeof globalThis.fetch,
		);
		try {
			const row: OAuthCredentials = { access: "a", refresh: "r", expires: 0 };
			await expect(refreshKiroHook(row)).rejects.toThrow(KIRO_OPT_IN_ENV);
			expect(fetchSpy).not.toHaveBeenCalled();
		} finally {
			fetchSpy.mockRestore();
		}
	});

	it("refuses to refresh an Identity Center or Builder ID login and sends no request", async () => {
		const fetchSpy = spyOn(globalThis, "fetch").mockImplementation(
			(async () => new Response("{}")) as unknown as typeof globalThis.fetch,
		);
		try {
			const row: OAuthCredentials = {
				access: "a",
				refresh: "r",
				expires: 0,
				kiroClientId: "client",
				kiroClientSecret: "secret",
				kiroOidcRegion: "us-east-1",
			};
			await expect(refreshKiroHook(row)).rejects.toThrow(KIRO_OPT_IN_ENV);
			expect(fetchSpy).not.toHaveBeenCalled();
		} finally {
			fetchSpy.mockRestore();
		}
	});
});
