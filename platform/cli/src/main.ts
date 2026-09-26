import { Command } from "commander";
import { api, printJson } from "./client.js";
import { loadConfig, saveConfig } from "./config.js";
import { promptHidden } from "./prompt.js";

const program = new Command("lightspeed-platform")
  .description("Lightspeed platform administration CLI")
  .configureHelp({ sortSubcommands: true });

program
  .command("login")
  .description("authenticate against the platform and store a bearer token")
  .requiredOption("--email <email>")
  .option("--url <url>", "platform base URL (persisted)")
  .option("--password <password>", "password (prompted when omitted)")
  .action(async (opts: { email: string; url?: string; password?: string }) => {
    const config = loadConfig();
    const baseUrl = opts.url ?? config.baseUrl;
    const password = opts.password ?? (await promptHidden("Password: "));
    const res = await fetch(new URL("/api/auth/sign-in/email", baseUrl), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        origin: new URL(baseUrl).origin,
      },
      body: JSON.stringify({ email: opts.email, password }),
    });
    const body = (await res.json()) as { token?: string };
    if (!res.ok) {
      console.error("Login failed:", JSON.stringify(body));
      process.exit(1);
    }
    // Bearer plugin surfaces the session token in the set-auth-token
    // header; recent versions also include it in the body.
    const token = res.headers.get("set-auth-token") ?? body.token;
    if (!token) {
      console.error("Login succeeded but no bearer token was returned.");
      process.exit(1);
    }
    saveConfig({ baseUrl, token });
    console.log(`Logged in to ${baseUrl}`);
  });

program
  .command("whoami")
  .description("show the authenticated user")
  .action(async () => printJson(await api("GET", "/api/v1/me")));

const universe = program.command("universe").description("manage universes");

universe
  .command("list")
  .action(async () => printJson(await api("GET", "/api/v1/universes")));

universe
  .command("create <name>")
  .option("--slug <slug>")
  .action(async (name: string, opts: { slug?: string }) =>
    printJson(
      await api("POST", "/api/v1/universes", {
        name,
        slug: opts.slug,
      }),
    ),
  );

universe
  .command("show <id>")
  .action(async (id: string) => printJson(await api("GET", `/api/v1/universes/${id}`)));

universe
  .command("archive <id>")
  .action(async (id: string) =>
    printJson(await api("PATCH", `/api/v1/universes/${id}`, { status: "archived" })),
  );

const member = program.command("member").description("manage universe members");

member
  .command("list")
  .requiredOption("--universe <id>")
  .action(async (opts: { universe: string }) =>
    printJson(await api("GET", `/api/v1/universes/${opts.universe}/members`)),
  );

member
  .command("add")
  .requiredOption("--universe <id>")
  .requiredOption("--user <userId>")
  .option("--role <role>", "viewer | contributor | operator | admin", "contributor")
  .action(async (opts: { universe: string; user: string; role: string }) =>
    printJson(
      await api("POST", `/api/v1/universes/${opts.universe}/members`, {
        userId: opts.user,
        role: opts.role,
      }),
    ),
  );

member
  .command("remove")
  .requiredOption("--universe <id>")
  .requiredOption("--member <memberId>")
  .action(async (opts: { universe: string; member: string }) =>
    printJson(
      await api(
        "DELETE",
        `/api/v1/universes/${opts.universe}/members/${opts.member}`,
      ),
    ),
  );

interface ChannelAccountDoc {
  accountId: string;
  provider: string;
  providerAccountId: string;
  displayName: string;
  credentialGrantId?: string | null;
  settings?: Record<string, unknown>;
  enabled?: boolean;
  revision: number;
}

async function setChannelAccountEnabled(
  universe: string,
  accountId: string,
  enabled: boolean,
): Promise<unknown> {
  const { account } = (await api(
    "GET",
    `/api/v1/universes/${universe}/channel-accounts/${accountId}`,
  )) as { account: ChannelAccountDoc };
  return api("PUT", `/api/v1/universes/${universe}/channel-accounts/${accountId}`, {
    account: {
      accountId: account.accountId,
      provider: account.provider,
      providerAccountId: account.providerAccountId,
      displayName: account.displayName,
      credentialGrantId: account.credentialGrantId ?? null,
      settings: account.settings ?? {},
      enabled,
    },
    expectedRevision: account.revision,
  });
}

const channelAccount = program
  .command("channel-account")
  .description("manage channel provider accounts (universe resources)");

channelAccount
  .command("list")
  .description("operator view: every account across universes")
  .action(async () => printJson(await api("GET", "/api/v1/channel-accounts")));

channelAccount
  .command("add")
  .requiredOption("--universe <id>")
  .requiredOption("--account-id <id>", "authored account id")
  .requiredOption("--provider <provider>", "channel provider slug (telegram, whatsapp, ...)")
  .requiredOption(
    "--provider-account-id <id>",
    "provider-native identity (bot username, phone number)",
  )
  .requiredOption("--display-name <name>")
  .option("--credential-grant <grantId>", "retrievable auth grant holding the provider token")
  .action(
    async (opts: {
      universe: string;
      accountId: string;
      provider: string;
      providerAccountId: string;
      displayName: string;
      credentialGrant?: string;
    }) =>
      printJson(
        await api("POST", `/api/v1/universes/${opts.universe}/channel-accounts`, {
          account: {
            accountId: opts.accountId,
            provider: opts.provider,
            providerAccountId: opts.providerAccountId,
            displayName: opts.displayName,
            credentialGrantId: opts.credentialGrant ?? null,
            settings: {},
          },
        }),
      ),
  );

channelAccount
  .command("enable <id>")
  .requiredOption("--universe <universeId>")
  .action(async (id: string, opts: { universe: string }) =>
    printJson(await setChannelAccountEnabled(opts.universe, id, true)),
  );

channelAccount
  .command("disable <id>")
  .requiredOption("--universe <universeId>")
  .action(async (id: string, opts: { universe: string }) =>
    printJson(await setChannelAccountEnabled(opts.universe, id, false)),
  );

channelAccount
  .command("rm <id>")
  .requiredOption("--universe <universeId>")
  .action(async (id: string, opts: { universe: string }) =>
    printJson(await api("DELETE", `/api/v1/universes/${opts.universe}/channel-accounts/${id}`)),
  );

const userCmd = program.command("user").description("manage platform users (admin)");

userCmd
  .command("create")
  .requiredOption("--email <email>")
  .requiredOption("--name <name>")
  .option("--role <role>", "user | admin", "user")
  .option("--password <password>", "prompted when omitted")
  .action(
    async (opts: { email: string; name: string; role: string; password?: string }) => {
      const password = opts.password ?? (await promptHidden("New user password: "));
      // better-auth admin plugin endpoint; bearer token must belong to a
      // platform admin.
      printJson(
        await api("POST", "/api/v1/admin/users", {
          email: opts.email,
          name: opts.name,
          password,
          role: opts.role,
        }),
      );
    },
  );

userCmd
  .command("list")
  .action(async () =>
    printJson(
      await api("GET", "/api/v1/admin/users"),
    ),
  );

const identity = program.command("identity").description("canonical deployment groups, memberships and role administration");
identity.command("list").action(async () => printJson(await api("GET", "/api/v1/admin/identity")));
identity.command("apply <json>").description("apply a core AccessChange JSON document")
  .action(async (json: string) => printJson(await api("POST", "/api/v1/admin/identity", JSON.parse(json))));

try {
  await program.parseAsync();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
