// app/tools/loginToSiteTool.ts
//
// "login_to_site" — agent-callable tool that drives a headless browser
// through a login flow. On success, the resulting cookies + localStorage
// are saved (encrypted) to the tenant's browser auth store. Subsequent
// `browse_web` calls for the same tenant come up already logged in.
//
// Credentials are NEVER persisted. They flow through the agent context,
// into computer-use-preview's screenshot reasoning, and out as keyboard
// events into the target site — exactly like a human typing them.
//
// 2FA flow:
//   1. Agent calls login_to_site without two_fa_code
//   2. If the site demands 2FA, the tool returns ok=false with
//      signedInIndicator="TWOFA_REQUIRED"
//   3. Agent surfaces a request to the user via Telegram message
//   4. User replies with the code
//   5. Agent calls login_to_site again with the same creds + two_fa_code
//      (the partial-login cookies from step 1 are still loaded, so the
//      browser is already at the 2FA prompt)

import { tool } from "ai";
import { z } from "zod/v4";

import { loginToSite } from "@/app/lib/sandboxBrowser";
import { recordCost } from "@/app/lib/costTracker";

export type LoginToSiteContext = {
  jobId?: string;
  // Tenant id is REQUIRED for this tool — without it we have no idea where
  // to save the harvested session. The makeLoginToSiteTool factory throws
  // if tenantId is missing at construction time (chat path supplies it via
  // userId composition).
  tenantId: string;
};

const APPROX_INPUT_TOKENS = 8000;
const APPROX_OUTPUT_TOKENS = 800;
const COMPUTER_USE_MODEL_FOR_BILLING = "gpt-5.4";

export function makeLoginToSiteTool(ctx: LoginToSiteContext) {
  if (!ctx.tenantId) {
    throw new Error("login_to_site requires a tenantId in context");
  }

  return tool({
    description: [
      "Log in to a website on behalf of the user using credentials the user",
      "has provided. After a successful login, the session is saved so future",
      "`browse_web` calls on the same site come up already logged in.",
      "",
      "Use this when:",
      "  - The user explicitly asked to log in to a site",
      "  - `browse_web` reported a login wall on a previously-unauthed site",
      "  - You need authenticated access to complete a goal",
      "",
      "Don't use this:",
      "  - Speculatively — the user must provide creds via chat first",
      "  - For sites where OAuth via Composio is available (prefer OAuth)",
      "",
      "2FA flow: if the site demands a code, this tool returns",
      "signedInIndicator='TWOFA_REQUIRED'. Ask the user for the code via a",
      "regular reply, then call login_to_site again with the same creds plus",
      "two_fa_code.",
      "",
      "Security: credentials flow through this call and through OpenAI's",
      "computer-use-preview model (which sees the typed values via screenshots).",
      "They are NOT stored anywhere by us — only the resulting session cookies.",
    ].join("\n"),
    inputSchema: z.object({
      login_url: z
        .string()
        .url()
        .describe("Direct URL to the site's login page."),
      username: z
        .string()
        .min(1)
        .describe("Username, email, or handle for the account."),
      password: z
        .string()
        .min(1)
        .describe("Password for the account."),
      two_fa_code: z
        .string()
        .nullable()
        .describe(
          "Two-factor code from the user's authenticator/SMS. Leave null on the first attempt; only populate after the tool reports TWOFA_REQUIRED."
        ),
      username_selector: z
        .string()
        .nullable()
        .describe(
          "Optional CSS selector hint for the username field. Usually unnecessary — the model finds the field from the screenshot."
        ),
      password_selector: z.string().nullable(),
      submit_selector: z.string().nullable(),
    }),
    execute: async (args) => {
      const result = await loginToSite({
        loginUrl: args.login_url,
        username: args.username,
        password: args.password,
        twoFaCode: args.two_fa_code ?? undefined,
        selectors: {
          username: args.username_selector ?? undefined,
          password: args.password_selector ?? undefined,
          submit: args.submit_selector ?? undefined,
        },
        tenantId: ctx.tenantId,
      });

      if (ctx.jobId) {
        await recordCost({
          jobId: ctx.jobId,
          model: COMPUTER_USE_MODEL_FOR_BILLING,
          usage: {
            inputTokens: APPROX_INPUT_TOKENS,
            outputTokens: APPROX_OUTPUT_TOKENS,
          },
        });
      }

      // Returns a result shape the calling LLM can reason about. Never
      // include the password in the return value (defense in depth).
      return {
        ok: result.ok,
        final_url: result.finalUrl,
        signed_in_indicator: result.signedInIndicator,
        ...(result.error ? { error: result.error } : {}),
      };
    },
  });
}
