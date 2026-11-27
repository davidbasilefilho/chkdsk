# disksonar

Painel de saúde de discos para Windows, escrito em Rust, com uma janela nativa em português brasileiro ou inglês, conforme a ordem dos idiomas de exibição do Windows, desenhada com [rune](https://github.com/anomalyco/rune). Consulta telemetria SMART/Health, apresenta o estado informado pelo diagnóstico e mantém histórico local para tendências de desgaste. O aplicativo não depende de serviços externos durante a execução.

> O disksonar consulta a telemetria de saúde dos discos. Para verificar ou reparar sistemas de arquivos, use o utilitário de sistema do Windows.

## Executar

A GUI requer Windows 10/11 com GPU compatível com DirectX 12 ou Vulkan. Basta abrir o executável: ele abre uma janela, sem console, que acompanha o tema claro ou escuro do sistema. À esquerda ficam os discos, com um ponto de cor para o estado de cada um; à direita, o painel do disco selecionado, com a resistência nominal restante quando medida, vida estimada, desgaste, temperatura, tempo ligado, última leitura e, em discos ATA, os atributos SMART brutos. Os discos são redescobertos a cada 15 segundos, com no máximo quatro consultas simultâneas em segundo plano. O runtime não requer Rust, mise ou acesso à rede.

O painel mostra a porcentagem de resistência restante quando há desgaste medido. Em SSDs NVMe, a origem é Percentage Used. Quando uma leitura direta está incompleta, o painel também consulta os contadores de confiabilidade do Windows associados ao mesmo caminho de disco. Um desgaste informado de zero é diferente de um campo ausente. A especificação pública do MACROVIP MV240GB não documenta as unidades dos atributos SMART por firmware. O valor bruto de 09 fica em **Informações avançadas**. [Ficha do fabricante](https://macrovip.com/en/produto/ssd-macrovip-premium-240gb-2-5-sata-3-mv240gb/)

Quando uma consulta requer privilégios adicionais, o aplicativo oferece uma tentativa de elevação pelo consentimento normal do Windows. Se o usuário cancelar, a janela continua disponível com os dados obtidos. As cenas de teste e capturas usam somente fixtures.

## Pontuação e limites

- A nota de 0 a 100 é determinística, mas não representa probabilidade de falha nem certificação do disco. Com evidência utilizável, notas acima de 50 são classificadas como saudáveis e notas até 50 como atenção; sem evidência utilizável, o estado é **Indisponível** e nenhuma nota é exibida.
- Para NVMe, a heurística considera percentual de desgaste usado, reserva disponível e avisos críticos reportados pelo dispositivo; condições críticas aplicam limites máximos à nota. Não há padrão portátil confiável para interpretar universalmente atributos ATA brutos: eles podem ser exibidos, mas não afetam a nota sem mapeamento específico e verificado por modelo.
- A ETA é uma extrapolação indicativa até 100% de desgaste usado, baseada no segmento atual de histórico desde a leitura mais antiga após o último reset/regressão do contador até a mais recente. Exige pelo menos 24 horas entre observações e aumento positivo de até dez pontos percentuais por dia; leituras iguais, regressivas ou dados insuficientes não estabelecem tendência. HDDs não recebem previsão. A ETA não prevê falhas nem garante vida útil.
- O histórico fica em `%APPDATA%\disksonar\historico.json` (ou na pasta temporária do usuário quando `%APPDATA%` não está definido). Na primeira execução sem histórico próprio, lê o histórico anterior de `%APPDATA%\chkdsk\historico.json` e salva as novas amostras em `disksonar`. O arquivo anterior é preservado. Mantém até 35 dias por disco: amostras por hora nos sete dias mais recentes e por dia antes disso. Uma regressão do contador de desgaste descarta o histórico anterior daquele disco para iniciar uma nova linha de base.
- O aplicativo não altera discos e normalmente não precisa de privilégios elevados. Windows, driver, controlador ou adaptador USB podem bloquear consultas SMART; elevar privilégios não torna disponível uma telemetria que o dispositivo ou controlador não fornece.

## Configurações

Abra **Configurações** na barra lateral ou no menu da janela compacta. Tema aceita Sistema, Claro e Escuro. Animações aceita Sistema, Ativadas e Desativadas. As duas preferências começam em Sistema e ficam em `%APPDATA%\disksonar\settings.json`. As alterações se aplicam na janela aberta. O idioma acompanha a preferência de exibição do Windows.

## Controles

- `←` / `→` : alternar entre discos.
- `↑` / `↓` (ou `k` / `j`): rolar o painel do disco; `Page Up`, `Page Down`, `Home` e `End` também rolam.
- Clique em um disco da lista para selecioná-lo; a roda do mouse rola o painel sob o ponteiro.
- `q` ou `Esc`: sair. Fechar a janela também encerra o aplicativo.

## Desenvolvimento

O ambiente de desenvolvimento usa [mise](https://mise.jdx.dev/) para provisionar Rust estável, `rustfmt`, Clippy, Bun para as tarefas TypeScript de build e release, `hk` e uma distribuição LLVM portátil fixada no projeto. LLVM não é necessário para executar o aplicativo. A compilação Windows requer os componentes do Visual Studio Build Tools (toolset MSVC e Windows SDK) para x64 e ARM64; mise instala os componentes Rust, mas não substitui o linker/SDK MSVC.

Na raiz do repositório, em PowerShell:

```powershell
mise install
mise exec -- cargo run
```

O build usa [rune](https://github.com/anomalyco/rune) fixado em um commit exato (`rev` em `Cargo.toml`); atualize-o de propósito, nunca por branch. Fora do Windows o aplicativo compila e os testes rodam, mas a lista de discos fica vazia (a coleta nativa é só para Windows). No Linux, o rune precisa de `libxkbcommon-dev` e `libwayland-dev`; para renderizar sem GPU, `mesa-vulkan-drivers` com `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`.

Capturas sem disco real (discos de exemplo em `src/fixtures.rs`; cenas: `saudavel`, `atencao`, `indisponivel`, `carregando`, `falha`, `vazio`, `procurando`, `descoberta`):

```powershell
cargo run --features scenes -- --snap captura.png --scene saudavel --size 1100x720 --scale 2 --theme dark
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

`validation` confere formatação, regras de release, linkers MSVC, Clippy, testes x64 e compilação nas duas arquiteturas. Se você já instalou o hook antigo, execute `prek uninstall` antes de migrar. `setup-hook` instala o pre-commit local do hk. A cada commit, ele executa `mise run fmt` e `mise run clippy` no projeto inteiro, inclusive quando apenas a documentação muda. `release` gera builds otimizados; as tarefas `release-arm64`, `release-x64` e `release-x64-baseline` compilam as duas interfaces de cada perfil.

## Releases

Tags estáveis seguem exatamente `v{versão}` e não aceitam versão SemVer pré-lançamento. Builds de desenvolvimento usam `v{versão}-dev-{sha7}`. Cada pacote contém três executáveis independentes com o prefixo `disksonar-{versão}[-dev-{sha7}]-` e um arquivo `SHA256SUMS.txt`.

| Perfil | Executável (sufixo) | Requisitos |
| --- | --- | --- |
| ARM64 | `windows-arm64.exe` | Windows ARM64, AArch64 com NEON; Rust usa o alvo MSVC ARM64 genérico. |
| x64 | `windows-x64.exe` | x86-64-v3 com AVX/AVX2 e suporte do sistema ao estado AVX. Inclui SSE3, SSSE3, SSE4.1/4.2, POPCNT, CMPXCHG16B, F16C, FMA, BMI1/2, LZCNT e MOVBE. |
| x64-baseline | `windows-x64-baseline.exe` | Alvo genérico x86-64 com SSE2; dispensa AVX. Aplicam-se também os requisitos mínimos do Windows instalado. |

Na dúvida sobre a CPU x64, escolha o baseline. Rust usa `-Ctarget-cpu=x86-64-v3` ou `-Ctarget-cpu=x86-64` em compilações separadas. O código C/C++ compilado com MSVC recebe `/arch:AVX2` ou `/arch:SSE2` para o mesmo alvo. Rotinas especializadas de dependências podem selecionar instruções adicionais em tempo de execução pela CPU. Bun executa as tarefas de desenvolvimento; os binários distribuídos são nativos Rust. [Perfis de geração de código do Rust](https://doc.rust-lang.org/rustc/codegen-options/index.html#target-cpu) · [Seleção de ISA no MSVC](https://learn.microsoft.com/en-us/cpp/build/reference/arch-x64)

Para ensaiar o pacote sem publicar, execute `mise exec -- bun scripts/release.ts package --mode dev --sha (git rev-parse HEAD) --output dist/release` no PowerShell. Use um diretório de saída vazio. O empacotamento e a publicação conferem os três nomes, seus hashes, os cabeçalhos PE, o subsistema gráfico e o perfil de CPU compilado. A CI do PR executa os arquivos empacotados com fixtures em runners Windows x64 e ARM64. Os fluxos dev e estável reutilizam essas verificações antes de publicar.

### Instalação por canal com mise

Em um checkout **confiável de `main`**, instale o mise e o GitHub CLI (`gh`). Autentique o CLI com acesso de leitura a Actions e Contents. Escolha o canal e o perfil de CPU.

```powershell
mise run channel-install latest x64-baseline
mise run channel-install dev arm64
mise run channel-install pr-13 x64
mise run channel-install pr x64-baseline
```

Cada comando verifica os três executáveis e o manifesto e instala o perfil escolhido em um diretório identificado pelo SHA, ID do artefato ou release e perfil de CPU. O instalador imprime um comando PowerShell `mise x 'github:davidbasilefilho/disksonar@path:…' -- disksonar.exe` com o caminho completo. O mise usa esse diretório mesmo quando o catálogo de versões ainda está desatualizado. Acrescente `--version` para conferir. Para atualizar, execute o instalador novamente. Para voltar, use a linha de uma instalação anterior. [Versões locais do mise](https://mise.jdx.dev/dev-tools/versions.html) · [API de artefatos do GitHub](https://docs.github.com/en/rest/actions/artifacts).

| Canal | Fonte verificada na instalação |
| --- | --- |
| `latest` | Ponteiro aprovado em `channels/latest.json` de `main`, tag estável, commit pertencente a `main` e release publicada com quatro assets (três executáveis e o manifesto). |
| `dev` | HEAD atual da branch real `dev`, com o build e os dois testes nativos verdes no evento **push** desse SHA. |
| `pr-N` | HEAD atual do PR aberto N, fora de rascunho, no próprio repositório, para `main` ou `dev`. Requer uma execução **pull_request** associada ao número, base e SHA. |
| `pr` | Entre os PRs elegíveis e verificados acima, o de maior data de **committer** do HEAD. Em caso de empate, vence o maior número de PR. |

Os canais de teste consultam a API em modo leitura. O identificador do artefato, o SHA completo e o digest SHA-256 do ZIP vinculam a instalação à execução. A resolução se repete após o download. Se outra execução, novo commit ou fechamento do PR mudar a seleção, a instalação para. Uma conclusão atrasada não escreve um alias remoto. Artefatos expirados e mudanças incompatíveis no workflow exigem nova validação ou atualização do checkout confiável. Uma versão instalada é identificada por SHA, ID do artefato/release, interface e perfil. A posição da release na página do GitHub e a data do upload não participam da seleção.

**Ativação de dev.** A branch existente `dev` tem histórico próprio. O canal acompanha somente o HEAD validado da branch dev. A publicação é feita pelo workflow confiável da branch padrão. Para rollback de `dev`, reverta em Git e valide o novo HEAD. A mesma regra vale para atualizar ou reverter o código de um PR.

**Promoção estável.** Configure o environment `stable` com aprovação obrigatória e restrição à branch `main`, proteja `main` e as tags estáveis contra reescrita. Depois de aprovação explícita, use uma tag SemVer estável no commit de `main` escolhido e revise o par tag/SHA em `channels/latest.json` via PR. Execute `stable release` manualmente a partir de `main` com esses valores. O job de escrita usa a fonte confiável do dispatch e confere a origem novamente antes de publicar. Para rollback do canal, revise o ponteiro em `main` para uma release anterior elegível. Isso não requer editar essa release. O ponteiro inicial conserva `v0.0.1`: a instalação de três variantes fica indisponível enquanto a estável aprovada usar o formato antigo.

**Pré-releases no GitHub.** Depois da validação completa, um workflow da branch padrão publica o pacote imutável como `v{versão}-dev-{sha40}` ou `v{versão}-pr-{número}-{sha40}`. Ele confere novamente o HEAD, a execução, o ZIP e o manifesto antes de publicar. PRs de forks, fechados ou em rascunho ficam fora. Para recuperar uma publicação pendente, o mesmo workflow aceita em main o ID e a tentativa da validação concluída, sem recompilar. O mise baixa os executáveis da pré-release do HEAD verificado, mesmo com várias pré-releases na página de Releases. O ZIP da CI só intermedeia a passagem do pacote já validado para o publicador e os testes nativos. [Gatilho e permissões do GitHub Actions](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_run)

**Teste.** `mise run release-check` cobre seleção, ordenação, ref/artefato obsoleto e o formato dos pacotes. Para um teste conectado, use `mise run channel-install pr-N x64-baseline` depois da CI verde e execute a linha impressa com `--scene saudavel --snap teste.png`. Os executáveis instaláveis vêm do GitHub Releases.

## Exportação e licença

Na barra de título, **Exportar** salva a leitura dos discos listados em JSON ou CSV. O JSON inclui identidade, fontes, atributos ATA e o histórico local. O CSV usa UTF-8 com BOM e protege campos informados pelo dispositivo contra interpretação como fórmulas de planilha. A exportação grava somente o arquivo escolhido no diálogo do Windows e mantém o destino intacto se a gravação falhar. Identificadores dos discos podem aparecer nesse arquivo quando o usuário decide exportar.

A sondagem ATA tenta ler os limites declarados pelo dispositivo e, em adaptadores USB compatíveis, pode consultar pelo protocolo SAT. Em modelos e firmwares sem mapeamento confirmado, os valores brutos continuam disponíveis na seção avançada e no JSON, sem inferir unidades ou datas. A leitura de temperatura também pode vir da propriedade de armazenamento do Windows.

O código é distribuído sob a [licença MIT](LICENSE).
