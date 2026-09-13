import { afterEach, describe, expect, it } from "bun:test";
import { configureCredentialRedaction, redactSensitiveCredentials } from "../src/providers/transform-messages";

// Every credential below is synthetic — AWS's own documentation example key,
// the RFC 7519 sample JWT, and truncated key armour with no real material.

afterEach(() => {
	configureCredentialRedaction(false);
});

describe("credential redaction", () => {
	it("is a pass-through until a host opts in", () => {
		// The default matters more than it looks: a host that never calls
		// configure gets the unmodified text, so this is what ships out of the
		// box and what the audit recorded.
		const text = "AKIAIOSFODNN7EXAMPLE";
		expect(redactSensitiveCredentials(text)).toBe(text);
	});

	it("redacts AWS access key ids, long-lived and temporary", () => {
		// `~/.aws/credentials` is plain text an agent reads without friction,
		// and the key id is the half that identifies the account.
		configureCredentialRedaction(true);
		expect(redactSensitiveCredentials("aws_access_key_id = AKIAIOSFODNN7EXAMPLE")).toBe(
			"aws_access_key_id = [aws_access_key_redacted]",
		);
		expect(redactSensitiveCredentials("ASIAY34FZKBOKMUTVV7A")).toBe("[aws_access_key_redacted]");
	});

	it("redacts a JWT without needing to know its issuer", () => {
		// Three base64url segments is the whole signal; a bearer token in a log
		// line carries no vendor prefix to match on.
		configureCredentialRedaction(true);
		const jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
		expect(redactSensitiveCredentials(`Authorization: Bearer ${jwt}`)).toBe("Authorization: Bearer [jwt_redacted]");
	});

	it("replaces a private key block whole, armour and body", () => {
		// A key spans lines and has no token shape, so it is matched by its
		// armour. Replacing the whole block matters: redacting only the parts
		// that look token-ish would leave a key that is still usable and a
		// marker suggesting it was handled.
		configureCredentialRedaction(true);
		const pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA1234\n-----END RSA PRIVATE KEY-----";
		expect(redactSensitiveCredentials(`key:\n${pem}\ndone`)).toBe("key:\n[private_key_redacted]\ndone");

		const openssh = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----";
		expect(redactSensitiveCredentials(openssh)).toBe("[private_key_redacted]");
	});

	it("leaves prose and identifiers that merely start with a prefix", () => {
		// False positives cost trust: a redactor that mangles source is turned
		// off, and then it protects nothing. `AKIA` needs its exact 16-character
		// tail to count.
		configureCredentialRedaction(true);
		for (const text of [
			"const region = AKIA_PREFIX_CONSTANT;",
			"see docs on AKIA keys",
			"eyJ is the base64 prefix for a JSON object",
		]) {
			expect(redactSensitiveCredentials(text)).toBe(text);
		}
	});

	it("keeps redacting the vendor tokens it already covered", () => {
		configureCredentialRedaction(true);
		expect(redactSensitiveCredentials(`ghp_${"a1B2".repeat(9)}`)).toBe("[github_token_redacted]");
		expect(redactSensitiveCredentials(`sk-ant-${"a1B2".repeat(9)}`)).toBe("[anthropic_token_redacted]");
	});
});
