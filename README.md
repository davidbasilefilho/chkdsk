# chkdsk

Painel de saúde de discos para Windows, escrito em Rust, com interface de terminal em português brasileiro (pt_BR). Consulta telemetria SMART/Health, apresenta uma nota heurística conservadora e mantém histórico local para tendências de desgaste. O aplicativo não depende de serviços externos durante a execução.

> Este projeto não é o `chkdsk` do Windows. Não verifica nem repara sistemas de arquivos, não agenda reparos e não substitui `chkdsk.exe`; também não executa varredura de superfície. Para evitar conflito com a ferramenta do sistema, inicie o executável deste projeto pelo caminho completo.

## Executar

Requer Windows 10/11 e um terminal compatível com TUI. Releases incluem executáveis para Windows x64-baseline, ARM64 e x86. O runtime não requer Rust, mise ou acesso à rede.

## Pontuação e limites

- A nota de 0 a 100 é determinística, mas não representa probabilidade de falha nem certificação do disco. Com evidência utilizável, notas acima de 50 são classificadas como saudáveis e notas até 50 como atenção; sem evidência utilizável, o estado é **SEM DADOS**.
- Para NVMe, a heurística considera percentual de desgaste usado, reserva disponível e avisos críticos reportados pelo dispositivo; condições críticas aplicam limites máximos à nota. Não há padrão portátil confiável para interpretar universalmente atributos ATA brutos: eles podem ser exibidos, mas não afetam a nota sem mapeamento específico e verificado por modelo.
- A ETA é uma extrapolação indicativa até 100% de desgaste usado, baseada no segmento atual de histórico desde a leitura mais antiga após o último reset/regressão do contador até a mais recente. Exige aumento positivo observado ao longo do tempo; leituras iguais, regressivas ou dados insuficientes não estabelecem tendência. HDDs não recebem previsão. A ETA não prevê falhas nem garante vida útil.
- O histórico fica em `%APPDATA%\chkdsk\historico.json` (ou na pasta temporária do usuário quando `%APPDATA%` não está definido). Mantém até 35 dias por disco: amostras por hora nos sete dias mais recentes e por dia antes disso. Uma regressão do contador de desgaste descarta o histórico anterior daquele disco para iniciar uma nova linha de base.
- O aplicativo não altera discos e normalmente não precisa de privilégios elevados. Windows, driver, controlador ou adaptador USB podem bloquear consultas SMART; elevar privilégios não torna disponível uma telemetria que o dispositivo ou controlador não fornece.

## Controles

- `←` / `→` ou `Tab` / `Shift+Tab`: alternar entre discos.
- `↑` / `↓`: rolar o conteúdo do painel; `j` / `k` também navegam pela lista de discos.
- Clique nas abas/lista de discos para selecionar e no controle de saída para sair; roda do mouse rola o conteúdo (sobre as abas, alterna discos).
- `q` ou `Esc`: sair.

## Desenvolvimento

O ambiente de desenvolvimento usa [mise](https://mise.jdx.dev/) para provisionar Rust estável, `rustfmt`, Clippy, Python 3.12, `prek` e uma distribuição LLVM portátil fixada no projeto. LLVM não é necessário para executar o aplicativo. A compilação Windows requer os componentes do Visual Studio Build Tools (toolset MSVC e Windows SDK) para x64, x86 e ARM64; mise instala os componentes Rust, mas não substitui o linker/SDK MSVC.

Na raiz do repositório, em PowerShell:

```powershell
mise install
mise exec -- cargo run
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

`validation` confere formatação, regras de release, linkers MSVC, Clippy, testes x64 e compilação nas três arquiteturas. `setup-hook` instala o hook local do `prek`, que verifica `rustfmt`. `release` gera builds otimizados; também existem `release-x64-baseline`, `release-arm64` e `release-x86`.

## Releases

Tags estáveis seguem exatamente `v{versão}` e não aceitam versão SemVer pré-lançamento. Builds de desenvolvimento usam `v{versão}-dev-{sha7}`. Os executáveis têm estes nomes (o sufixo `-dev-{sha7}` aparece somente em builds de desenvolvimento):

```text
chkdsk-{versão}[-dev-{sha7}]-windows-x64-baseline.exe
chkdsk-{versão}[-dev-{sha7}]-windows-arm64.exe
chkdsk-{versão}[-dev-{sha7}]-windows-x86.exe
```

`mise run release-check` valida localmente regras SemVer, nomes dos artefatos e mapeamentos de arquitetura sem publicar.
