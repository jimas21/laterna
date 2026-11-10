// The relay Laterna sends a crash report to when a user chooses to.
// It files each report as an issue in a private GitHub repository, so the
// token that can do that stays here instead of in the app. Set up as
// docs/DEVELOPMENT.md (Crash reports) describes; the secret GITHUB_TOKEN may
// only write issues of REPO.

const MAX_BODY = 96 * 1024;
// GitHub takes issue bodies up to 65536 characters.
const MAX_TEXT = 60000;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.pathname !== "/report") {
      return new Response("Not found", { status: 404 });
    }
    // The address only counts reports here; it is not kept.
    if (env.LIMITER) {
      const key = request.headers.get("cf-connecting-ip") ?? "unknown";
      const { success } = await env.LIMITER.limit({ key });
      if (!success) {
        return new Response("Too many reports", { status: 429 });
      }
    }
    const raw = await request.text();
    if (raw.length > MAX_BODY) {
      return new Response("Too large", { status: 413 });
    }
    let report;
    try {
      report = JSON.parse(raw);
    } catch {
      return new Response("Not JSON", { status: 400 });
    }
    const { app, version, summary, text } = report ?? {};
    if (app !== "frameplayerx" || ![version, summary, text].every((v) => typeof v === "string")) {
      return new Response("Not a Laterna crash report", { status: 400 });
    }
    // The end of the log is where the crash is.
    const kept = text.length > MAX_TEXT ? "…" + text.slice(-MAX_TEXT) : text;
    // A fence longer than any run of backticks in the report.
    const longest = Math.max(0, ...(kept.match(/`+/g) ?? []).map((run) => run.length));
    const fence = "`".repeat(Math.max(3, longest + 1));
    const issue = {
      title: `${version.slice(0, 40)}: ${summary.slice(0, 200)}`,
      body: `Sent from Laterna after a crash.\n\n${fence}text\n${kept}\n${fence}\n`,
    };
    const reply = await fetch(`https://api.github.com/repos/${env.REPO}/issues`, {
      method: "POST",
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${env.GITHUB_TOKEN}`,
        "User-Agent": "frameplayerx-crash-relay",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      body: JSON.stringify(issue),
    });
    if (!reply.ok) {
      console.error(`GitHub answered ${reply.status}: ${await reply.text()}`);
      return new Response("Could not file the report", { status: 502 });
    }
    return new Response("Thank you", { status: 201 });
  },
};
