import { describe, expect, it } from "vitest";
import {
  collectOwnWhatsAppJids,
  stripOwnWhatsAppMentions,
  whatsappMentionsOwnIdentity,
} from "../src/whatsapp.js";

describe("WhatsApp own identity mentions", () => {
  it("matches native mentions against the bot LID", () => {
    const ownJids = collectOwnWhatsAppJids(
      {
        id: "41793353447:3@s.whatsapp.net",
        lid: "73092474884270:3@lid",
      },
      "41793353447",
    );

    expect(whatsappMentionsOwnIdentity(["73092474884270@lid"], ownJids)).toBe(true);
  });

  it("strips the numeric LID mention from the prompt", () => {
    const ownJids = collectOwnWhatsAppJids(
      {
        id: "41793353447:3@s.whatsapp.net",
        lid: "73092474884270:3@lid",
      },
      "41793353447",
    );

    expect(
      stripOwnWhatsAppMentions(
        "@73092474884270 can you add entries to our calendar?",
        ["73092474884270@lid"],
        ownJids,
      ),
    ).toBe("can you add entries to our calendar?");
  });

  it("keeps unrelated numeric mentions", () => {
    const ownJids = collectOwnWhatsAppJids(
      {
        id: "41793353447:3@s.whatsapp.net",
        lid: "73092474884270:3@lid",
      },
      "41793353447",
    );

    expect(
      stripOwnWhatsAppMentions("@12345 are you coming?", ["12345@lid"], ownJids),
    ).toBe("@12345 are you coming?");
  });
});
