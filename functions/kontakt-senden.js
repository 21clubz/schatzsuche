/**
 * Nimmt das Kontaktformular von /kontakt entgegen und schickt den Inhalt
 * als E-Mail weiter. Läuft als Cloudflare Pages Function unter
 * /kontakt-senden.
 *
 * Der eigene Pfad ist kein Schmuck: Bei Pages haben statische Dateien Vorrang
 * vor Functions. Läge die Datei hier `kontakt.js`, würde `kontakt.html` sie
 * verdecken und ein Absenden mit „405 Method Not Allowed" enden.
 *
 * ACHTUNG, noch nicht scharf: Dieses Verzeichnis wird derzeit nicht gebaut, ein
 * POST auf /kontakt-senden endet mit 404. Der Grund ist die Projektart — das
 * Cloudflare-Projekt ist ein Worker mit statischen Dateien, kein
 * Pages-Projekt, und `functions/` ist eine reine Pages-Vorrichtung. Nötig ist
 * eine `wrangler.jsonc` mit `main` auf ein Worker-Skript und `assets` auf
 * `docs`; dann zieht der Inhalt hier dorthin um. Bis dahin zeigt das Formular
 * ehrlich die E-Mail-Adresse als Ausweichweg. Schritte in
 * ~/Desktop/schatzsuche-kontaktformular-anleitung.md.
 *
 * Gespeichert wird hier nichts — weder die Nachricht noch die IP-Adresse. Was
 * ankommt, geht direkt weiter und ist danach nur noch im Postfach.
 *
 * Nötige Einstellungen im Pages-Projekt:
 *   BREVO_API_KEY   (Secret)  — der Schlüssel von Brevo
 *   KONTAKT_AN      (optional) — Empfängeradresse, sonst die unten voreingestellte
 *   KONTAKT_VON     (optional) — Absenderadresse, muss bei Brevo bestätigt sein
 */

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

export async function onRequestPost({ request, env }) {
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

/** Wer /kontakt direkt aufruft, landet auf dem Formular. */
export async function onRequestGet() {
  return Response.redirect("https://schatzsuche-bitcoin.com/kontakt", 302);
}
