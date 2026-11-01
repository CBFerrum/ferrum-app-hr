import { describe, expect, it } from "vitest";
import { EMPTY, looksLikeEmail, looksLikeLink, meta, payload, problems } from "./careers";

const filled = { ...EMPTY, name: "Asha Rao", email: "asha@example.com" };

describe("the application form", () => {
  it("asks for a name and an address, and nothing else", () => {
    expect(problems(filled)).toEqual({});
    expect(Object.keys(problems(EMPTY)).sort()).toEqual(["email", "name"]);
  });

  it("tells what is wrong next to the field", () => {
    expect(problems({ ...filled, email: "asha" }).email).toBe("This does not look like an email address");
    expect(problems({ ...filled, resume_link: "example.com/cv" }).resume_link).toMatch(/https/);
    expect(problems({ ...filled, resume_link: "https://example.com/cv.pdf" })).toEqual({});
    expect(problems({ ...filled, phone: "1".repeat(41) }).phone).toMatch(/40/);
    expect(problems({ ...filled, cover_letter: "x".repeat(5001) }).cover_letter).toMatch(/5000/);
  });

  it("takes an address as the server does", () => {
    expect([looksLikeEmail("a@b.co"), looksLikeEmail(" a@b.co "), looksLikeEmail("a@b"), looksLikeEmail("a b@c.de"), looksLikeEmail("@c.de"), looksLikeEmail("a@@c.de")]).toEqual([
      true,
      true,
      false,
      false,
      false,
      false,
    ]);
    expect([looksLikeLink("https://x.test/a"), looksLikeLink("http://x.test"), looksLikeLink("javascript:alert(1)"), looksLikeLink("ftp://x.test")]).toEqual([true, true, false, false]);
  });

  it("sends what was filled in, trimmed", () => {
    expect(payload({ ...filled, name: "  Asha Rao ", phone: " " })).toEqual({ name: "Asha Rao", email: "asha@example.com" });
    expect(meta({ department: "Support", branch: null, employment_type: "Full-time" })).toBe("Support · Full-time");
  });
});
