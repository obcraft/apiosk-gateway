# Gateway bouwen en uitrollen

De gateway is één Rust-server met de provider- en betaalhandlers uit de
workspacebibliotheek `crates/commerce-runtime`. Eén repository en één
lockbestand leveren één serverbinary en één serverproces op. De entrypoint
vervangt zichzelf met dat proces, zodat Fly-signalen rechtstreeks bij de
gateway aankomen. Het operatorhulpmiddel `check-brightdata-linkedin` blijft in
de image beschikbaar en wordt niet automatisch gestart.

`fly.toml` blijft de runtimeconfiguratie voor `apiosk-gateway-v2`. De bestaande
buyer-ledger en treasurybetaalgrenzen blijven intact. `EXECUTOR_DATABASE_URL`
blijft de configuratienaam van de afzonderlijke databasepool van de
commercebibliotheek, zodat de bestaande databaserechten behouden blijven;
deze variabele start geen tweede proces of listener.

## Build en cache

De Dockerfile gebruikt Rust **1.97.1** en één cargo-chef-recept voor de volledige
gateway-workspace, inclusief `commerce-runtime` en `nl-benefits`. De dependencybuild staat vóór het
kopiëren van de echte broncode. Een bronwijziging kan daardoor de gecompileerde
dependencies hergebruiken; een gewijzigd manifest, lockbestand, buildprofiel of
toolchain veroorzaakt terecht een nieuwe dependencybuild. `--locked` voorkomt
dat de build dependencies ongemerkt bijwerkt.

GitHub Actions bouwt op `ubuntu-24.04` voor `linux/amd64` en bewaart ook de
tussenlagen in de GitHub Actions-cache (`mode=max`, scope
`gateway-linux-amd64-v2`). Deze laagcache overleeft een nieuwe runner. De eerste
build en een build na cacheverwijdering blijven volledige builds. Een snellere
warme build is pas aangetoond als de log de dependencylagen als `CACHED` toont;
een lange compilatie op zichzelf bewijst geen compilerfout of Fly-storing.

Cargo-tests hebben daarnaast hun eigen cache voor de volledige workspace. De
release-image krijgt de volledige gatewaycommit in `GATEWAY_GIT_SHA`. Die waarde
is ook zichtbaar op `/health`.

Na de Rust-suite start `scripts/test-native-runtime.mjs` de native runtime
tegen een lokale PGlite-databasefixture. De controle dekt startup, health,
authenticatie en de bestaande quote- en featuregrenzen. De runner gebruikt een
afgebakende testomgeving zonder productiecredentials of externe providers.
Node 24 en `npm ci --ignore-scripts` installeren daarvoor de vastgelegde
testdependencies uit `package-lock.json`.

## GitHub Actions instellen

**Gateway CI** (`.github/workflows/gateway.yml`) bouwt en test.
**Gateway release** (`.github/workflows/gateway-release.yml`) rolt een al
geverifieerde image uit. Externe actions staan op vaste commithashes en flyctl
op **0.5.0**.

Voor build en tests zijn geen eigen secrets nodig. Eén normale checkout bevat
de volledige workspace; de pipeline haalt geen andere repository of submodule
op. Alleen deployment vereist `FLY_API_TOKEN`: een deploy token beperkt tot
`apiosk-gateway-v2`, als secret in de GitHub-omgeving `production` of als
repositorysecret. Alleen de push/deploy-stap in de production-job ontvangt dit token.

Een gewone push naar `main`, een pull request of een handmatige **Gateway CI**-run
voert de Rust-controles, imagebuild en imageverificatie uit. CI zelf rolt niets
naar Fly uit; na een push naar `main` doet **Gateway release** dat. Pull requests gebruiken dezelfde controles zonder productiecredentials.
Alleen een CI-run op `main` bewaart de geverifieerde image als
`gateway-image-<volledige commit>`, gedurende zeven dagen. De release gebruikt
dit artifact alleen wanneer de volledige CI-run succesvol is afgerond,
inclusief de Rust-suite en native-runtimetest.

## Een productieversie uitrollen

Elke push naar `main` start **Gateway release** automatisch. De release wacht op
de geslaagde **Gateway CI**-run van exact die commit en rolt die daarna uit;
faalt CI, dan wordt niets uitgerold. Een lokale commit publiceert niets: pas de
push naar GitHub start de uitrol. Om `main` opnieuw uit te rollen, open
**Actions → Gateway release → Run workflow** en selecteer `main`. Een release
gebruikt altijd precies de commit waarop die run is gestart.

De workflow:

1. Zoekt met `scripts/find-verified-image.py` een succesvolle main-CI-run van
   exact dezelfde commit. De selectie controleert workflow, repository,
   branch, resultaat en het niet-verlopen image-artifact. Deze job heeft alleen
   leesrechten op GitHub en ontvangt geen Fly-token.
2. Wacht zo nodig maximaal twintig minuten op een bestaande CI-run. De
   release start geen nieuwe build en voert geen Rust-tests opnieuw uit.
3. Controleert na een eventuele productiegoedkeuring opnieuw dat dezelfde
   CI-run, poging en hetzelfde artifact nog geldig zijn. Downloadt dat artifact
   via zijn vaste run- en artifact-ID,
   laadt de image en voert `scripts/verify-image.sh` opnieuw uit voor exact
   dezelfde commit. Een artifact van een andere commit wordt niet gebruikt.
4. Pusht de image naar de Fly-registry, bepaalt de immutable digest en roept
   `scripts/deploy-image.py` aan met die digest en de verwachte commit. Vlak
   daarvoor controleert de selectie opnieuw dat `main`, de succesvolle
   CI-poging en het artifact niet zijn veranderd.
5. Controleert de release via de bestaande healthroute. Een succesvolle HTTP
   response moet ook de juiste commit en gereedstaande commercehandlers
   tonen; alleen een voltooid buildlog is onvoldoende.

Als CI faalt, ontbreekt of het artifact ouder dan zeven dagen is, stopt de
release vóór de production-job. Start dan **Gateway CI** handmatig op `main`
en start de release opnieuw zodra die run geslaagd is. Een beschikbare image
gaat rechtstreeks naar verificatie en rollout; compilatie en tests zijn al
tijdens CI uitgevoerd.

De rollout bewaart machinecontroles en releasegegevens als artifact
`gateway-release-<commit>`; ook een mislukte uitrol probeert dit bewijs te
bewaren. Voor de rollout controleert het script onder meer de actuele
runtime-instellingen en de vorige image-digest, zodat configuratiedrift niet
stilzwijgend wordt meegenomen.

Productiedeployments worden geserialiseerd. Een lopende deployment wordt niet
afgebroken door een volgende workflowrun. Vlak vóór de registry-push en de
rollout moet de releasecommit nog steeds de actuele `main` zijn. Een oudere
release die in de wachtrij stond, stopt wanneer `main` ondertussen verder is.
Na een push stopt die zonder fout, omdat de release van de nieuwere push die
commit uitrolt; een handmatige run stopt met een fout. Een bewuste rollback gebruikt de hieronder beschreven herstelprocedure.
De bestaande Fly-runtimeconfiguratie,
betaalflags en secrets blijven behouden. De pipeline voert geen aankopen of
databasewijzigingen uit.

## Lokaal controleren

Voer vanuit de gatewaydirectory uit:

Vereist Rust 1.97.1, Python 3.11+, Node 24 met npm, Docker en `pdftotext` voor de PDF-fixtures
(`poppler-utils` op Ubuntu, `brew install poppler` op macOS). De workflow
installeert deze testdependency zelf. Replays gebruiken uitsluitend opgeslagen
fixtures; het testscript schakelt live replays en het herschrijven van tapes uit.

```sh
bash scripts/ci-check.sh
npm ci --ignore-scripts
node scripts/test-native-runtime.mjs
docker buildx build --platform linux/amd64 --load \
  --build-arg "GIT_SHA=$(git rev-parse HEAD)" \
  --tag apiosk-gateway:local .
bash scripts/verify-image.sh apiosk-gateway:local "$(git rev-parse HEAD)"
```

Deze controles gebruiken geen productiecredentials. De bestaande
`scripts/test-release-gate.mjs` bevat ook live plannercontroles; die hoort niet
bij deze uitsluitend op build en lokale tests gerichte pipeline.

## Diagnose en herstel

Controleer eerst welke fase faalt: repositorycheckout, Rust-tests,
dependencycompilatie, applicatiecompilatie, artifactselectie, imageverificatie, registry-push of
Fly-rollout. De logs van deze stappen staan afzonderlijk in Actions. Bewaar bij
een mislukte rollout de vorige image-digest en de bijbehorende gatewaycommit.
Iedere release bevat de volledige server; een afzonderlijke executor-update
is niet meer beschikbaar.

Rollback is een bewuste uitrol van een eerdere bekende image, zonder nieuwe
build. Gebruik daarbij de runtimeconfiguratie die bij die release hoort; een
oude image combineren met gewijzigde `fly.toml`-instellingen kan ander gedrag
opleveren. Voer daarna dezelfde controle op commit en `payment_executor` uit.
Bij de eerste migratie naar één serverproces heeft de vorige image ook zijn
vastgelegde `EMBEDDED_EXECUTOR_ORIGIN` en `EMBEDDED_EXECUTOR_PORT` nodig. De
rollbackopdracht in het releasebewijs herstelt deze instellingen wanneer ze
bij de vorige image aanwezig waren. Zie [de cutoverprocedure](production-cutover.md).
Verander tijdens rollback geen betaaladministratie, secrets of migraties om
een healthcheck te laten slagen.

De implementatie volgt de
[cargo-chef-laagcache](https://github.com/LukeMathWalker/cargo-chef), de
[Docker GitHub Actions-cache](https://docs.docker.com/build/cache/backends/gha/)
en de [Fly GitHub Actions-deployment](https://docs.fly.io/launch/continuous-deployment-with-github-actions).
