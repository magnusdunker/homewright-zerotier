# ZeroTier til TheHome

En ekstern integration til [TheHome](https://github.com/magnusdunker/thehome), der viser dine ZeroTier-netværk og deres medlemmer fra ZeroTier Central (my.zerotier.com). Med den kan du se, hvilke maskiner der er online, og godkende eller afvise medlemmer.

## Installation

I TheHome: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/thehome-zerotier
```

og tryk **Installér**. Tilføj derefter **ZeroTier** under **Tilføj integration**.

Integrationen skal bruge et API-token. ZeroTier har to udgaver af Central, og begge virker:

- **Nye Central** (central.zerotier.com): lav en service account under **Organization → Service Accounts**, og lav et API-token til den. ZeroTier kræver mindst Essential-abonnementet for det.
- **Ældre Central** (my.zerotier.com): lav et token under **Account → API Access Tokens**.

Integrationen finder selv ud af, hvilken af dem tokenet hører til. Har kontoen flere netværk, vælger du bagefter, hvilke der skal med. Står alle valgt, kommer netværk, du laver senere, også med. Valget kan ændres under integrationens indstillinger.

## Hvad du får

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

- `src/api.rs`: ZeroTier Central-API'et
- `src/model.rs`: netværk og medlemmer, og hvordan de bliver til tilstande
- `src/lib.rs`: forbindelsen til TheHome
- `tests/`: tests mod en falsk ZeroTier Central, og en falsk TheHome i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.2.0 && git push origin v0.2.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne på releasen.

## Licens

MIT
