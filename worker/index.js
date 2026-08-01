/**
 * Das Programm hinter schatzsuche-bitcoin.com.
 *
 * Es tut genau eine Sache selbst: Es nimmt das Kontaktformular von /kontakt
 * entgegen und schickt den Inhalt als E-Mail weiter. Alles andere reicht es
 * unverändert an die Seiten im Ordner `docs` durch.
 *
 * Warum überhaupt ein Worker-Skript: Dieses Projekt ist ein **Worker mit
 * statischen Dateien**, kein Pages-Projekt. Ein Ordner `functions/` und ein
 * `_worker.js` sind Pages-Vorrichtungen — beide werden hier nie ausgeführt,
 * sondern höchstens als Text ausgeliefert. Die Zuordnung steht in
 * `wrangler.jsonc`.
 *
 * Gespeichert wird hier nichts — weder die Nachricht noch die IP-Adresse. Was
 * ankommt, geht direkt weiter und ist danach nur noch im Postfach.
 *
 * Nötige Einstellungen im Projekt:
 *   BREVO_API_KEY   (Secret)   — der Schlüssel von Brevo
 *   KONTAKT_AN      (optional) — Empfängeradresse, sonst die unten voreingestellte
 *   KONTAKT_VON     (optional) — Absenderadresse, muss bei Brevo bestätigt sein
 */

const PFAD = "/kontakt-senden";
const AN_VOREINSTELLUNG = "schatzsuche-bitcoin@proton.me";
const VON_VOREINSTELLUNG = "kontakt@schatzsuche-bitcoin.com";

/** Eine Adresse muss nicht perfekt sein, aber wie eine aussehen. */
function adresseWirktEcht(wert) {
  return /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/.test(wert) && wert.length <= 200;
}

/**
 * Antwortet passend zum Absender: fetch bekommt JSON, ein Browser ohne
 * JavaScript eine lesbare Seite.
 */
function antwort(anfrage, status, ok, text) {
  const willJson = (anfrage.headers.get("Accept") || "").includes("application/json");
  if (willJson) {
    return new Response(JSON.stringify(ok ? { ok: true } : { ok: false, fehler: text }), {
      status,
      headers: { "content-type": "application/json; charset=utf-8" },
    });
  }
  const seite = `<!doctype html><html lang="de"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${ok ? "Angekommen" : "Nicht geklappt"} — Schatzsuche</title>
<link rel="icon" type="image/png" href="/assets/favicon.png">
<link rel="stylesheet" href="/assets/recht.css"></head><body>
<div class="topbar"><a href="/"><img src="/assets/icon.png" alt="" width="512" height="512">
<span class="wordmark">SCHATZSUCHE</span></a></div>
<main><h1>${ok ? "Angekommen" : "Das hat nicht geklappt"}</h1>
<div class="meldung ${ok ? "gut" : "schlecht"}">${text}</div>
<p style="margin-top:1.6rem"><a href="/kontakt">← Zurück zum Formular</a></p></main>
</body></html>`;
  return new Response(seite, {
    status,
    headers: { "content-type": "text/html; charset=utf-8" },
  });
}

async function nachrichtAnnehmen(request, env) {
  let formular;
  try {
    formular = await request.formData();
  } catch {
    return antwort(request, 400, false, "Die Anfrage war nicht lesbar.");
  }

  // Der Honigtopf ist für Menschen unsichtbar. Ist er ausgefüllt, war es ein
  // Bot — der bekommt ein freundliches Ja und die Nachricht wird verworfen.
  if ((formular.get("webseite") || "").toString().trim() !== "") {
    return antwort(request, 200, true, "Danke für deine Nachricht.");
  }

  const name = (formular.get("name") || "").toString().trim().slice(0, 100);
  const email = (formular.get("email") || "").toString().trim();
  const nachricht = (formular.get("nachricht") || "").toString().trim();

  if (!adresseWirktEcht(email)) {
    return antwort(request, 400, false, "Diese E-Mail-Adresse sieht nicht richtig aus.");
  }
  if (nachricht.length < 5) {
    return antwort(request, 400, false, "Die Nachricht ist zu kurz.");
  }
  if (nachricht.length > 5000) {
    return antwort(request, 400, false, "Die Nachricht ist zu lang — bitte auf 5000 Zeichen kürzen.");
  }

  if (!env.BREVO_API_KEY) {
    // Ohne Schlüssel kann nichts verschickt werden. Der Hinweis nennt den
    // Weg, der immer funktioniert, statt den Fehler zu verschweigen.
    return antwort(request, 500, false,
      "Der Versand ist gerade nicht eingerichtet. Schreib bitte direkt an " + AN_VOREINSTELLUNG + ".");
  }

  const an = env.KONTAKT_AN || AN_VOREINSTELLUNG;
  const von = env.KONTAKT_VON || VON_VOREINSTELLUNG;

  const versand = await fetch("https://api.brevo.com/v3/smtp/email", {
    method: "POST",
    headers: {
      "api-key": env.BREVO_API_KEY,
      "content-type": "application/json",
      accept: "application/json",
    },
    body: JSON.stringify({
      sender: { name: "Schatzsuche Kontaktformular", email: von },
      to: [{ email: an }],
      replyTo: { email, name: name || email },
      subject: "Schatzsuche — Nachricht über das Kontaktformular",
      textContent:
        "Name:    " + (name || "— nicht angegeben —") + "\n" +
        "E-Mail:  " + email + "\n" +
        "\n" + nachricht + "\n",
    }),
  });

  if (!versand.ok) {
    return antwort(request, 502, false,
      "Der Versand hat gerade nicht funktioniert. Schreib bitte direkt an " + AN_VOREINSTELLUNG + ".");
  }

  return antwort(request, 200, true,
    "Angekommen. Ich melde mich in der Regel innerhalb eines Werktags.");
}

export default {
  async fetch(request, env) {
    const pfad = new URL(request.url).pathname;

    if (pfad === PFAD) {
      if (request.method === "POST") return nachrichtAnnehmen(request, env);
      // Wer den Endpunkt von Hand aufruft, landet auf dem Formular.
      return Response.redirect(new URL("/kontakt", request.url).toString(), 302);
    }

    // Alles andere ist eine ganz normale Seite. Statische Dateien werden
    // ohnehin vor diesem Skript bedient; hierher kommt nur, wofür es keine
    // Datei gibt — und dann soll die übliche 404-Seite antworten.
    return env.ASSETS.fetch(request);
  },
};
