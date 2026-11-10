# chkdsk

Painel de saúde de discos para Windows, escrito em Rust, com uma janela nativa em português brasileiro (pt_BR), desenhada com [rune](https://github.com/anomalyco/rune). Consulta telemetria SMART/Health, apresenta uma nota heurística conservadora e mantém histórico local para tendências de desgaste. O aplicativo não depende de serviços externos durante a execução.

> Este projeto não é o `chkdsk` do Windows. Não verifica nem repara sistemas de arquivos, não agenda reparos e não substitui `chkdsk.exe`; também não executa varredura de superfície. Para evitar conflito com a ferramenta do sistema, inicie o executável deste projeto pelo caminho completo.

## Executar

Requer Windows 10/11 com GPU compatível com DirectX 12 ou Vulkan. Basta abrir o executável: ele abre uma janela, sem console, que acompanha o tema claro ou escuro do sistema. À esquerda ficam os discos, com um ponto de cor para o estado de cada um; à direita, o painel do disco selecionado, com a nota em destaque, vida estimada, desgaste, temperatura, tempo ligado, última leitura, os avisos de saúde e, em discos ATA, os atributos SMART com limites e valores brutos. Os discos são redescobertos a cada 15 segundos, com no máximo quatro consultas simultâneas em segundo plano. Cada release traz três executáveis: `x64` (compilado para `x86-64-v3`, exige CPU com AVX2, a partir de Haswell/Excavator), `x64-baseline` (`x86-64-v2`, para qualquer CPU aceita pelo Windows 11 24H2 e para as que não têm AVX2) e `arm64`. Na dúvida, use `x64-baseline`. Não há build de 32 bits (o rune não compila para i686) nem `arm64-baseline`. O runtime não requer Rust, mise ou acesso à rede.

## Pontuação e limites

- A nota de 0 a 100 é determinística e heurística: não é probabilidade de falha, certificação, medição de desgaste nem previsão de vida restante. **Saudável** exige evidência utilizável, nota acima de 50 e nenhum aviso; **Atenção** vale para falha crítica declarada pelo dispositivo ou por um limiar pré-falha, para nota até 50 e para sinais degradados, e a janela lista cada motivo; **Indisponível** significa que não há evidência suficiente para calcular uma nota, e nenhuma nota é exibida (o zero interno nunca aparece).
- Para NVMe, a nota considera o desgaste usado (limita a nota a `100 − desgaste`), a reserva disponível e os avisos críticos do dispositivo; condições críticas aplicam limites máximos. Erros de mídia (`media_errors` acima de zero) limitam a nota a 70 e deduzem até 30 pontos por ordem de grandeza decimal; o bit 5, de região de memória persistente, limita a nota a 70. Esses dois sinais avisam, mas isoladamente não marcam falha. O tipo do disco, quando a descoberta não o identifica, vem de um log NVMe lido com sucesso.
- Para ATA/SATA, o aplicativo consulta SMART READ DATA e tenta ler SMART READ THRESHOLDS, por pass-through ATA nativo e com o IOCTL SMART legado como alternativa quando a resposta falha ou vem malformada. Os atributos são mostrados com valores normalizados atuais e piores, limite declarado e valor bruto. Um limiar atingido limita a nota a 20; só vira falha quando a flag pré-falha (bit 0) está marcada ou o dispositivo relata falha global. Ficar até 10 pontos normalizados acima do limiar limita a nota a 70 e gera aviso, sem declarar falha. Linhas com ID duplicado são ambíguas e não entram na avaliação. Um status global SMART de “sem falha”, sozinho, não produz nota.
- Para HDD, há um índice de contadores brutos somente para os IDs ATA comuns 5 (setores realocados), 10 (tentativas de giro), 187 (erros incorrigíveis reportados), 197 (setores pendentes) e 198 (setores incorrigíveis offline). O ID 5 só é usado quando a codificação raw16 é inequívoca; se uma codificação necessária for ambígua, a nota do HDD fica indisponível (salvo se outra evidência crítica independente já determinar atenção), mas os avisos interpretáveis continuam. Por contador não zero, a dedução heurística é `base + coeficiente × floor(log10(contagem))`, limitada pelo teto; as deduções se combinam de forma multiplicativa, `nota × (100 − dedução)/100` para cada classe:

  | ID ATA | Sinal | Base | Coeficiente | Teto |
  | --- | --- | ---: | ---: | ---: |
  | 5 | Setores realocados | 4 | 3 | 25 |
  | 10 | Tentativas de giro | 10 | 4 | 35 |
  | 187 | Erros incorrigíveis reportados | 20 | 8 | 55 |
  | 197 | Setores pendentes | 25 | 10 | 65 |
  | 198 | Setores incorrigíveis offline | 30 | 10 | 70 |

  Os pesos são escolhas heurísticas do projeto, não calibração empírica nem probabilidade; contadores relacionados podem penalizar o mesmo problema duas vezes. Um contador não zero reduz a nota e avisa, mas não prova falha física.
- Em SSD, o desgaste usado do NVMe e o atributo ATA 202 nos perfis exatos de Crucial MX300, MX500 e BX500 limitam a nota a `100 − desgaste`. O perfil lê o valor bruto de 48 bits como percentual usado e limita valores acima de 100 a 100%; assim, 100% de desgaste dá nota zero e aviso, sem classificar o disco como falho. Não há mapeamento verificado para outros fabricantes (como o atributo 177 da Samsung) nem inferência de vida útil a partir de outros IDs brutos.
- Temperatura e horas ligadas só aparecem quando decodificadas por um layout suportado: o log de saúde NVMe e o perfil estreito WD10EZEX (ID 194 para a temperatura e ID 9 para as horas, com valor bruto compatível). Outros atributos ATA, IDs duplicados e codificações desconhecidas permanecem indisponíveis.
- A ETA de desgaste é uma extrapolação indicativa até 100% de desgaste usado, baseada no segmento atual de histórico desde a leitura mais antiga após o último reset/regressão do contador até a mais recente. Exige aumento positivo observado ao longo do tempo; leituras iguais, regressivas ou dados insuficientes não estabelecem tendência. HDDs não recebem previsão. A ETA não prevê falhas nem garante vida útil.
- O histórico fica em `%APPDATA%\chkdsk\historico.json` (ou na pasta temporária do usuário quando `%APPDATA%` não está definido). Mantém até 35 dias por disco: amostras por hora nos sete dias mais recentes e por dia antes disso. Uma regressão do contador de desgaste descarta o histórico anterior daquele disco para iniciar uma nova linha de base.
- O aplicativo não altera discos. Ao abrir normalmente, pede elevação pelo UAC do Windows e se reinicia como administrador, uma única vez, após a confirmação; se a elevação for cancelada, ele avisa e fecha. `chkdsk.exe --version` só imprime a versão, e as capturas sem janela (`--snap`) não pedem elevação. Windows, driver, controlador ou adaptador USB ainda podem bloquear consultas SMART; elevar privilégios não torna disponível uma telemetria que o dispositivo ou controlador não fornece.

## Controles

- `←` / `→` ou `Tab` / `Shift+Tab`: alternar entre discos.
- `↑` / `↓` (ou `k` / `j`): rolar o painel do disco; `Page Up`, `Page Down`, `Home` e `End` também rolam.
- Clique em um disco da lista para selecioná-lo; a roda do mouse rola o painel sob o ponteiro.
- `q` ou `Esc`: sair. Fechar a janela também encerra o aplicativo.

## Desenvolvimento

O ambiente de desenvolvimento usa [mise](https://mise.jdx.dev/) para provisionar Rust estável, `rustfmt`, Clippy, Python 3.12, `prek` e uma distribuição LLVM portátil fixada no projeto. LLVM não é necessário para executar o aplicativo. A compilação Windows requer os componentes do Visual Studio Build Tools (toolset MSVC e Windows SDK) para x64 e ARM64; mise instala os componentes Rust, mas não substitui o linker/SDK MSVC.

Na raiz do repositório, em PowerShell:

```powershell
mise install
mise exec -- cargo run -p chkdsk
```

O build usa [rune](https://github.com/anomalyco/rune) fixado em um commit exato (`rev` em `[workspace.dependencies]` do `Cargo.toml` da raiz); atualize-o de propósito, nunca por branch. Fora do Windows o aplicativo compila e os testes rodam, mas a lista de discos fica vazia (a coleta nativa é só para Windows). No Linux, o rune precisa de `libxkbcommon-dev` e `libwayland-dev`; para renderizar sem GPU, `mesa-vulkan-drivers` com `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`.

O código é um workspace Cargo em `crates/`: `chkdsk-core` (modelo, leitura dos dados SMART/NVMe, pontuação de saúde e histórico; lógica pura, sem dependências de plataforma), `chkdsk-windows` (acesso nativo aos discos e elevação UAC, com stubs fora do Windows) e `chkdsk` (o executável: coleta, textos, janela e as cenas de exemplo). Comandos que valem para tudo usam `--workspace`, por exemplo `cargo test --workspace`.

Capturas sem disco real (discos de exemplo em `crates/chkdsk/src/fixtures.rs`; cenas: `saudavel`, `atencao`, `indisponivel`, `carregando`, `falha`, `vazio`, `procurando`, `descoberta`):

```powershell
cargo run -p chkdsk --features scenes -- --snap captura.png --scene saudavel --size 1100x720 --scale 2 --theme dark
```

Validação e compilação:

```powershell
mise run fmt
mise run clippy
mise run tests
mise run compile
mise run validation
mise run setup-hook
mise run release
```

`validation` confere formatação, regras de release, linkers MSVC, Clippy, testes x64 e compilação nas duas arquiteturas (no Windows, com o MSVC). `setup-hook` instala o hook local do `prek`, que verifica `rustfmt`. `release` gera os três executáveis otimizados em `dist/release` com os nomes de um build de desenvolvimento; também existem `release-x64`, `release-x64-baseline` e `release-arm64`. Cada variante usa o próprio diretório `target/<variante>`, porque `x64` e `x64-baseline` diferem em `-C target-cpu` e não podem compartilhar artefatos.

No Linux (e no CI), `mise run ci-linux` roda formatação, a autoverificação de `scripts/release.py`, Clippy no host, Clippy para `x86_64` e `aarch64` MSVC com [cargo-xwin](https://github.com/rust-cross/cargo-xwin) (`mise run clippy-xwin`) e `cargo test --workspace` (`mise run tests-host`). Isso exige `clang`, `lld`, `llvm` e as bibliotecas do rune descritas acima; o cargo-xwin baixa o CRT e o SDK do Windows na primeira execução. O perfil `release` usa LTO completo (`fat`), uma unidade de codegen e `strip` (menor executável medido: 19,7 MB contra 21,6 MB do perfil padrão), mantendo `panic = "unwind"` (o coletor depende de `catch_unwind`).

### CI e fluxo de branches

Todo PR vai para `dev`; só `dev` vai para `main`. Os merges são squash, então o workflow de release registra `main` em `dev` com um merge `-s ours` (veja abaixo). Nenhum outro branch dispara workflows, e PRs cujo branch de origem é `holyagent/**` ou de checkpoint (`checkpoint/**`, `*/checkpoint/*`) não executam nada.

| Workflow | Quando | O que faz |
| --- | --- | --- |
| `dev` | push em `dev`; PR para `dev` ou `main` | `plan` decide o que rodar; `validation`; `prerelease` (no PR para `main`, só depois do job `version`); `checks` reúne o resultado de todos |
| `validation` | chamado por `dev` e `stable` | `linux` (fmt, Clippy host e xwin, testes, autoverificação) e `windows` (testes no Windows) |
| `release` | chamado por `dev` e `stable` | `config` (decide a assinatura), `build` (x64, x64-baseline e arm64, em paralelo), `sign` (quando configurado), `publish` |
| `stable` | push em `main` | `plan`, `validation`, `release` e `backmerge` |

O job `plan` pula tudo quando o PR só muda documentação (`*.md`, `docs/`, `LICENSE`) em relação à base do PR inteiro (`pull_request.base.sha`, não só o último push), quando um push em `dev` já é coberto por um PR aberto de `dev` e, nos pré-releases, para PRs de forks e commits que já têm release. A verificação é feita dentro dos jobs, e não por `paths-ignore`, para que o check obrigatório continue reportando.

**Check obrigatório.** Configure como obrigatório em `dev` e em `main` somente o check `checks` (job `checks` do workflow `dev`, exibido como `dev / checks (pull_request)`). Ele roda sempre e só fica verde quando `plan` teve sucesso e `version`, `validation` e `prerelease` tiveram sucesso ou foram pulados de propósito (segundo as saídas de `plan`); falha ou cancelamento de qualquer job o deixa vermelho. Não exija `validation / linux`, `validation / windows` nem `version` separadamente: eles não existem quando o job é pulado, e um check obrigatório ausente bloqueia o PR para sempre.

**Concorrência.** Cada push em `dev` e em `main` tem o próprio grupo (por SHA), então nenhuma execução é cancelada nem descartada da fila. Um PR cancela as execuções anteriores do mesmo PR; como a publicação é retomável (rascunho primeiro, assets verificados antes de publicar, rascunho existente reaproveitado), cancelar uma execução durante a publicação deixa no máximo um rascunho não publicado, que a poda nunca apaga.

#### Assinatura com SignPath (opcional)

O job `config` decide a assinatura uma vez, e a publicação exige essa decisão:

- `signed`: push em `dev` ou `main` com a variável `SIGNPATH_ORGANIZATION_ID` e o segredo `SIGNPATH_API_TOKEN` definidos. O job `sign` envia os três `.exe` ao [SignPath](https://signpath.io) antes de gerar os checksums, e se a assinatura falhar ou for pulada nada é publicado.
- `unconfigured`: nem a variável nem o segredo existem. Os executáveis saem sem assinatura (também em releases estáveis) e as notas do release dizem isso. Definir só um dos dois faz o job `config` falhar.
- `disabled`: execuções de PR rodam código não revisado; nunca assinam e nunca recebem o segredo (o workflow `dev` o repassa vazio fora de pushes), e as notas do release dizem que o build não é assinado.

Em *Settings > Secrets and variables > Actions*, defina:

- variável `SIGNPATH_ORGANIZATION_ID`: ID da organização;
- variável `SIGNPATH_PROJECT_SLUG`: slug do projeto;
- variável `SIGNPATH_SIGNING_POLICY_SLUG`: slug da política de assinatura;
- variável `SIGNPATH_ARTIFACT_CONFIGURATION_SLUG`: slug da configuração de artefato (um zip com `*.exe` na raiz);
- segredo `SIGNPATH_API_TOKEN`: token da API.

No SignPath, o projeto precisa confiar neste repositório como *trusted build system* (GitHub.com). PRs, de forks ou não, nunca assinam.

## Releases

O projeto segue [ZeroVer](https://0ver.org/): a versão major é sempre `0` (`0.y.z`, sem sufixos), e `[workspace.package] version` no `Cargo.toml` da raiz é a única fonte da versão.

- **Pré-releases.** Cada commit de um push em `dev` ou da ponta de um PR (para `dev` ou `main`) gera, no máximo uma vez, um pré-release `v{versão}-dev.{AAAA-MM-DD}.{HH-MM-SS}.g{sha7}` (por exemplo `v0.1.0-dev.2026-11-10.12-45-02.g50eb54a`). Data e hora são as do commit, em UTC; como os campos têm largura fixa, a ordem alfabética e a ordem de precedência SemVer são cronológicas dentro de uma mesma versão, e todos ficam abaixo da tag estável `v{versão}` dessa versão. Depois de publicar, o workflow apaga os pré-releases de desenvolvimento já publicados (e suas tags) além dos 10 mais recentes; o release que acabou de ser publicado, os rascunhos e os releases estáveis nunca são apagados.
- **Versão.** Um PR para `main` só pode sair de `dev` e precisa elevar a versão para um valor ZeroVer maior que a última tag estável `v*`; o job `version`, que o check `checks` exige, falha caso contrário.
- **Release estável.** O merge em `main` cria a tag `v{versão}` (se ainda não existir), compila, verifica, assina (se configurado; sem SignPath configurado, publica sem assinatura e avisa nas notas) e publica automaticamente. Se a tag já existe em outro commit, o release é recusado. Em seguida, o job `backmerge` faz `git merge -s ours` do commit de release em `dev`; se `dev` não aceitar o push de `github-actions`, o job falha com instruções.

Os executáveis têm estes nomes (o sufixo `-dev.{AAAA-MM-DD}.{HH-MM-SS}.g{sha7}` aparece somente em builds de desenvolvimento) e vêm com `SHA256SUMS.txt`:

```text
chkdsk-{versão}[-dev.{AAAA-MM-DD}.{HH-MM-SS}.g{sha7}]-windows-x64.exe
chkdsk-{versão}[-dev.{AAAA-MM-DD}.{HH-MM-SS}.g{sha7}]-windows-x64-baseline.exe
chkdsk-{versão}[-dev.{AAAA-MM-DD}.{HH-MM-SS}.g{sha7}]-windows-arm64.exe
```

`mise run release-check` valida localmente as regras de tag, ZeroVer, planejamento, o check obrigatório, poda, notas de assinatura, `--locked` e nomes de artefatos, sem publicar.

## Licença

Este projeto é distribuído sob a licença MIT. Veja o arquivo [LICENSE](LICENSE).
