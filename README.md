# Schatzsuche

**Ein Programm, das zufällige Bitcoin-Wallets errät — und dabei beweist, dass das
nicht funktioniert.**

Schatzsuche würfelt BIP-39-Seeds aus echter Betriebssystem-Entropie, leitet für
jeden die BIP-44/49/84-Adressen ab und prüft sie gegen eine lokale Liste von
Adressen mit Guthaben. Dann rechnet es aus, wie lange ein Treffer dauern würde,
und zeigt das Ergebnis in **Vielfachen des Alters des Universums** an.

Auf einem Apple M1 sind das rund 10¹⁹ Universumsalter. Diese Zahl ist der Punkt
des Programms.

> Es findet nichts. Nicht selten — nie. Wer etwas anderes verspricht, verkauft
> dir etwas.

## Warum das kein Angriffswerkzeug ist

Die Suche ist **gleichverteilt über den gesamten Schlüsselraum**. Die Entropie
kommt direkt aus `getrandom`, nichts wird aus einem Pseudozufallsgenerator
expandiert, und kein Seed wird in Richtung schwacher oder bekannt kaputter
Generatoren gelenkt.

Genau das ist der Unterschied. Werkzeuge, die tatsächlich fremde Wallets leeren,
suchen nicht zufällig — sie zielen auf Brainwallets, defekte Zufallsgeneratoren
oder Wortlisten. Schatzsuche tut nichts davon und wird es nicht tun.

Wer den Beweis will, findet ihn in `src/engine.rs`: Entropie rein, nichts
dazwischen.

## Installation

Fertige Programme für macOS, Linux und Windows liegen unter
[Releases](../../releases). Herunterladen, entpacken, starten.

Selbst bauen:

```bash
cargo build --release
```

Getestet wurde gegen **Linux x86-64** und **Windows x86-64** per
Cross-Kompilierung mit Zig; beide Binaries entstehen fehlerfrei. Jeder Commit
wird zusätzlich auf echten Linux-, macOS- und Windows-Rechnern gebaut und
getestet.

Auf dem eigenen Rechner lohnt sich der Prozessor-spezifische Build — rund 13 %
mehr Tempo, dafür läuft die Datei nur auf dieser CPU-Generation:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

Unter Linux werden dafür die GUI-Bibliotheken gebraucht:

```bash
sudo apt install libgtk-3-dev libxkbcommon-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libssl-dev libdbus-1-dev
```

## Erste Schritte

```bash
schatzsuche init-config
schatzsuche synth-db --count 5000000
schatzsuche --test-persistence
schatzsuche --gui
```

`synth-db` baut eine synthetische Datenbank, damit die ganze Kette ohne echten
Dump läuft. Mit einem echten Dump funktionierter Adressen stattdessen:

```bash
schatzsuche build-db --input dump.tsv
```

Ohne Argumente startet das Programm im Terminal. Unter macOS öffnet der
Doppelklick auf das App-Bundle ein Fenster; auf allen Systemen erzwingt `--gui`
das Fenster.

## Bedienung

| | |
| --- | --- |
| Leertaste, Knopf START/STOPP | Suche anhalten und fortsetzen |
| ↑ ↓, Klick auf einen Treffer | Treffer auswählen |
| q, Esc | Beenden |

Ein ausgewählter Treffer zeigt seine Wörter nummeriert im rechten Feld.

## Leistung und Stromverbrauch

Kernanzahl, Priorität und Adressen pro Wallet lassen sich **im laufenden
Betrieb** ändern. Statt den Thread-Pool neu zu bauen, parkt sich ein Worker
oberhalb der eingestellten Kernzahl selbst — über dieselbe Bremse, die auch die
Pause benutzt.

Gemessen auf einem **unbelasteten** M1 (4 Leistungs- + 4 Effizienzkerne),
Seeds/s:

| Kerne | Sparsam | Normal | Maximal |
| --- | --- | --- | --- |
| 1 | 191 | 605 | 605 |
| 2 | 320 | 1019 | 1210 |
| 4 | 510 | 1211 | 2425 |
| 6 | 573 | 1337 | 2961 |
| 8 | 509 | 1401 | **3343** |

Drei Dinge daran sind nicht offensichtlich:

* **„Normal" deckelt bei etwa 42 %**, egal wie viele Kerne. macOS hält diese
  Stufe von den Leistungskernen fern.
* **„Sparsam" wird bei acht Kernen langsamer als bei sechs** — die Arbeit ist
  auf vier Effizienzkerne beschränkt, die sich dann gegenseitig behindern.
* **Nur „Maximal" skaliert.** Vier Kerne liefern 72 % der Spitze, weshalb das
  die Voreinstellung ist: die halbe Maschine bleibt frei.

Die Oberfläche interpoliert ihre Schätzung aus dieser Tabelle und warnt, wenn die
gewählte Kombination sich selbst im Weg steht.

Eine frühere Fassung dieser Tabelle entstand, während das Programm selbst im
Hintergrund lief, und war durchgehend falsch — sie unterschätzte die Spitze um
40 %. Ein Benchmark auf einer beschäftigten Maschine misst die Maschine, nicht
den Code.

Speicherbedarf: rund 25 MB für den Suchfilter, plus die Datenbankdatei, die
speicherabgebildet und damit auslagerbar ist. `bloom_fpr` in `config.toml`
tauscht Filtergenauigkeit direkt gegen Arbeitsspeicher.

Die Priorität wirkt unter macOS und Linux. Windows kennt die Einstellung
derzeit nicht; die Oberfläche sagt das, statt es zu verschweigen.

**Windows ist langsamer.** Die handgeschriebene SHA-512-Kompression aus
`sha2-asm` — auf Apple-Silicon rund 40 % des Gesamtdurchsatzes — unterstützt
Windows nicht; das Paket bricht den Build ab, statt zurückzufallen. Dort läuft
PBKDF2 mit der portablen Implementierung. Gemessener Unterschied auf einem M1:
1400 gegen 1020 Seeds/s bei sonst gleichen Bedingungen.

## Wohin ein Treffer geht

Die Reihenfolge ist ein Haltbarkeitsversprechen, kein Stilmittel:

1. Vollständiger Datensatz nach `hits.jsonl` — Wörter, Entropie, privater
   Schlüssel, Pfad, Adresse, Betrag, Zeitstempel, Rechnername.
2. Rechte auf 0600, dann `F_FULLFSYNC` auf die Datei **und** ein `fsync` auf das
   Verzeichnis. Unter macOS lässt `fsync(2)` die Daten im flüchtigen
   Schreibpuffer der SSD; `F_FULLFSYNC` ist die einzige echte Barriere.
3. Zweite Kopie nach `hits_backup.jsonl` (am besten auf ein anderes Laufwerk).
4. Rote Zeile in der Oberfläche, Wörter im Klartext.
5. Terminal-Glocke.
6. *Erst danach* die Benachrichtigungskette.
7. Die Suche läuft weiter.

## Der Seed verlässt den Rechner nie

`AlertPayload` hat **kein Feld**, das ein Mnemonic aufnehmen könnte. Das ist
durch den Typ erzwungen, nicht durch Disziplin an jeder einzelnen Stelle — ntfy,
Telegram und SMTP laufen über fremde Server, und ein Seed in einer
Push-Nachricht ist ein Seed auf fremder Infrastruktur, dauerhaft.

Benachrichtigungen enthalten: Zeitstempel, Rechnername, Ableitungspfad,
Skripttyp, Adresse, Betrag und den Hinweis, dass der Seed lokal liegt. Der Test
`alert_payload_never_contains_seed` bricht den Build, falls sich das je ändert.

Zusätzlich verweigert das Programm den Start, wenn ntfy aktiviert ist und das
Thema noch auf dem Beispielwert steht oder zu kurz ist. ntfy-Themen sind
öffentlich — jeder, der den Namen errät, liest mit.

## Benachrichtigungen

Fünf Kanäle über das `Notifier`-Trait: ntfy, Telegram, SMTP, generischer
JSON-Webhook und lokale Desktop-Meldung. Aktivierung in `config.toml`.

* Alle Kanäle feuern gleichzeitig, je ein Thread, mit festen Zeitlimits. Ein
  toter Kanal verzögert nur sich selbst.
* Exponentieller Backoff, fünf Versuche pro Kanal.
* Scheitern *alle*, landet die Nachricht in `pending_alerts.jsonl` und wird alle
  60 Sekunden erneut versucht, bis einer durchkommt.
* Dedupliziert über die Treffer-ID, damit Wiederholungen keine Lawine auslösen.

Vor dem Vertrauen einmal mit `--test-alert` durchspielen.

## Wo die Zeit hingeht

`schatzsuche bench` misst jede Stufe einzeln. Auf einem M1 mit 20 Adressen pro
Pfad:

| Stufe | Zeit | Anteil |
| --- | --- | --- |
| PBKDF2-HMAC-SHA512, 2048 Runden | 702 µs | 28 % |
| Elliptische Kurve + BIP-32 | 1811 µs | 72 % |
| Alles andere | ~1 µs | 0 % |

**PBKDF2 ist bei dieser Einstellung nicht der Flaschenhals.** Es dominiert nur
unterhalb von etwa 8 Adressen pro Pfad; darüber kosten die 66
`ecmult_gen`-Aufrufe je Seed mehr. `bench` gibt diesen Umschlagpunkt für die
eigene Konfiguration aus.

Zwei gemessene Optimierungen:

| Schritt | Seeds/s pro Kern | Änderung |
| --- | --- | --- |
| Ausgangspunkt | 270 | — |
| `sha2/asm` (Hardware-SHA-512 der M1) | 351 | +30 % |
| Public Keys nur bei Bedarf (BIP-32) | 398 | +13 % |

Gehärtete Ableitung braucht nie den öffentlichen Schlüssel des Elternknotens —
ihn trotzdem zu berechnen war reine Verschwendung, 10 von 73
`ecmult_gen`-Aufrufen pro Seed.

## Nachschlagen

Zwei Stufen. Ein cache-line-blockierter Bloom-Filter (16 Spuren à 32 Bit = ein
ausgerichteter 64-Byte-Block, ein Bit je Spur, also **ein** Cache-Fehlzugriff
statt sechzehn), danach bei einem Treffer binäre Suche in einer
speicherabgebildeten sortierten Datei.

Die Filtergröße wird numerisch gegen ein Modell gelöst, das über die
poissonverteilte Blockbelegung mittelt. Nach der Lehrbuchformel — die bei der
mittleren Belegung rechnet — liegt die echte Rate um etwa Faktor 100 daneben,
weil die Rate konvex in der Belegung ist und die vollen Blöcke dominieren.

Gemessen gegen 5 Mio. Einträge: 2 Fehlalarme auf 2,39 Mio. Abfragen (8,4e-7) bei
1e-6 Ziel, alle von der Plattenstufe verworfen.

## Tests

```bash
cargo test --release
```

92 Tests. Die tragenden:

* BIP-39/32/44/49/84-Ableitung gegen die Referenzbibliotheken `bitcoin` und
  `bip39` über zufällige Seeds, plus die veröffentlichten Testvektoren.
* `alert_payload_never_contains_seed` — die Garantie, dass nichts durchsickert.
* `bloom_survives_structured_keys` — Regressionstest für einen Fehler in der
  Sondenverteilung, der die Fehlalarmrate auf 16 % hochtrieb.
* `placeholder_ntfy_topic_is_rejected` — der Start muss verweigert werden.
* `synthetic_hits_are_labelled_not_celebrated` — ein Selbsttest-Eintrag darf nie
  wie ein echter Fund aussehen.
* `workers_above_the_active_count_park` — die Kernsteuerung im laufenden Betrieb.
* Oberflächen werden abseits des Bildschirms gerendert und auf Inhalt geprüft.

Jeder Commit wird unter Linux, macOS und Windows gebaut, getestet und geprüft.

## Lizenz

MIT. Siehe [LICENSE](LICENSE).

---

*[@21clubz](https://x.com/21clubz)*
