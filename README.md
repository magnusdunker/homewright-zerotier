# ZeroTier til TheHome

En ekstern integration til [TheHome](https://github.com/magnusdunker/thehome), der kan to ting:

- **Denne maskine** (standard): TheHome bliver selv medlem af et ZeroTier-netværk, så du kan nå TheHome over ZeroTier. Du skriver kun netværks-id'et og godkender TheHome i ZeroTier Central, som du plejer. Der skal ikke installeres noget på maskinen, og der skal intet API-token til.
- **ZeroTier Central (API-token)**: viser dine netværk og deres medlemmer fra ZeroTier Central, med en kontakt, der godkender hvert medlem.

## Installation

I TheHome: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/thehome-zerotier
```

og tryk **Installér**. Tilføj derefter **ZeroTier** under **Tilføj integration**, og vælg **Denne maskine** eller **ZeroTier Central API token**.

## Denne maskine

1. Skriv netværks-id'et (16 tegn, står under netværket i ZeroTier Central) og den port, TheHome kører på. Porten er udfyldt med TheHomes egen, hvis TheHome kører med `THEHOME_PORT` (som i Docker-installationen), og ellers 8123.
2. Integrationen starter sin ZeroTier-node og melder den ind i netværket. Formularen viser nodens adresse, 10 tegn, f.eks. `366859034f`.
3. Åbn netværket i ZeroTier Central, find medlemmet med den adresse, og godkend det.
4. Når noden er godkendt, kan TheHome nås på nodens ZeroTier-IP og porten, f.eks. `http://10.147.17.5:8124`.

ZeroTier er bygget ind i integrationen med [libzt](https://github.com/zerotier/libzt), som har sin egen TCP/IP-stak. Den skal ikke have et netværkskort, root eller ekstra rettigheder i Docker. Nodens identitet ligger i integrationens datamappe, så adressen er den samme efter genstart og opdateringer.

Fordi forbindelserne ender i integrationen og ikke i maskinens netværk, sender integrationen dem videre til TheHome på `127.0.0.1` og porten. Det er almindelig TCP, så WebSockets virker også. Under integrationens indstillinger kan du ændre porten og tilføje flere porte på maskinen, der også skal kunne nås, f.eks. `1883` for MQTT.

Maskinen skal kunne nå internettet over UDP. ZeroTier bruger port 9993 og tilfældige porte.

Du får to enheder:

- **ZeroTier-node `<adresse>`**: **Node-adresse**, **Online** (om noden har forbindelse til ZeroTier) og **ZeroTier-version**
- **ZeroTier `<netværk>`**:
  - **Forbundet**: til, når noden er godkendt og har fået netværkets indstillinger
  - **Status**: `JOINING`, `REQUESTING_CONFIGURATION`, `ACCESS_DENIED` (venter på godkendelse), `OK`, `NOT_FOUND`, `PORT_ERROR`, `CLIENT_TOO_OLD` eller `left`. Attributterne har en dansk forklaring, netværkets navn og type, MAC-adressen og hvad der videresendes
  - **ZeroTier-IP**: nodens adresse i netværket, alle adresser i `ip_assignments`
  - **Medlem af netværket**: en kontakt, der melder noden ud og ind. Valget huskes efter genstart
  - **Aktive forbindelser**: hvor mange forbindelser der er videresendt lige nu

Venter noden mere end et minut på godkendelse, viser TheHome et problem med adressen, der skal godkendes. Det forsvinder, når noden er godkendt.

Opsætningens navn får netværkets navn, når noden er godkendt. Fjerner du opsætningen i TheHome, er noden stadig medlem i ZeroTier Central. Fjern den der, hvis den ikke skal bruges igen.

## ZeroTier Central (API-token)

Her skal integrationen bruge et API-token. ZeroTier har to udgaver af Central, og begge virker:

- **Nye Central** (central.zerotier.com): lav en service account under **Organization → Service Accounts**, og lav et API-token til den. ZeroTier kræver mindst Essential-abonnementet for det.
- **Ældre Central** (my.zerotier.com): lav et token under **Account → API Access Tokens**.

Integrationen finder selv ud af, hvilken af dem tokenet hører til. Har kontoen flere netværk, vælger du bagefter, hvilke der skal med. Står alle valgt, kommer netværk, du laver senere, også med. Valget kan ændres under integrationens indstillinger.

Hvert netværk bliver en enhed med:

- antal godkendte medlemmer, der er online
- antal godkendte medlemmer
- netværkets id og navn og det samlede antal medlemmer som attributter

Hvert medlem bliver en enhed under sit netværk med:

- **Online**: til, når medlemmet har kontaktet ZeroTier inden for de seneste 5 minutter
- **Godkendt**: en kontakt, der godkender medlemmet på netværket eller fjerner godkendelsen
- **Sidst set**: tidspunktet for medlemmets seneste kontakt med ZeroTier
- **ZeroTier-IP**: den første tildelte adresse. Alle adresser står i attributten `ip_assignments`
- **Klientversion**: den ZeroTier-version, medlemmet kører. Den er slået fra, når den oprettes

Den offentlige adresse, medlemmet sidst kontaktede ZeroTier fra, står som attributten `physical_address` på **Online**.

Data hentes hvert minut. Nye medlemmer får entities ved næste hentning. Medlemmer, der er fjernet fra netværket, bliver utilgængelige. Medlemmer, der er skjult på my.zerotier.com, tages ikke med.

ZeroTier skriver selv, at tidspunkterne for seneste kontakt kan nulstilles uden varsel. Så viser **Online** slukket og **Sidst set** ukendt, indtil medlemmet har været i kontakt igen.

Afviser ZeroTier tokenet, viser TheHome det under problemer, og integrationen kan godkendes igen med et nyt token.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [thehome-example-integration-rust](https://github.com/magnusdunker/thehome-example-integration-rust).

- `src/zt.rs`: libzt, ZeroTier som bibliotek
- `src/proxy.rs`: videresendelse fra ZeroTier-adressen til TheHome
- `src/node.rs`: "Denne maskine": noden, netværket og entities
- `src/api.rs`: ZeroTier Central-API'et
- `src/model.rs`: netværk og medlemmer fra Central, og hvordan de bliver til tilstande
- `src/lib.rs`: opsætningen og forbindelsen til TheHome
- `vendor/libzt/`: libzts kildekode, se `vendor/libzt/VENDORED.md`
- `build.rs`: bygger libzt med `cc`
- `tests/`: tests mod en falsk ZeroTier og en falsk ZeroTier Central, og en falsk TheHome i `tests/host`
- `examples/adhoc.rs`: en rigtig test af videresendelsen over et ZeroTier ad-hoc-netværk, der ikke skal godkendes

```
cargo test
cargo build --release
```

Bygningen kræver en C- og C++-kompiler. Linux-binærerne bygges statisk med musl i `messense/rust-musl-cross`, f.eks.:

```
docker run --rm -v "$PWD:/src" -w /src messense/rust-musl-cross:aarch64-musl cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.2.0 && git push origin v0.2.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne på releasen.

## Licens

Integrationens egen kode er MIT. Binærerne indeholder libzt og ZeroTier fra ZeroTier, Inc. De er udgivet under Business Source License 1.1 med en Change Date (2026-01-01 for libzt og 2025-01-01 for ZeroTier-koden i den), hvorefter licensen for de versioner skifter til Apache License 2.0. Se `vendor/libzt/LICENSE.txt`. lwIP er BSD-licenseret.
