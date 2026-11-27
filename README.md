# chkdsk

Painel de saúde de discos para Windows, escrito em Rust, com uma janela nativa em português brasileiro (pt_BR), desenhada com [rune](https://github.com/anomalyco/rune). Consulta telemetria SMART/Health, apresenta uma nota heurística conservadora e mantém histórico local para tendências de desgaste. O aplicativo não depende de serviços externos durante a execução.

> Este projeto não é o `chkdsk` do Windows. Não verifica nem repara sistemas de arquivos, não agenda reparos e não substitui `chkdsk.exe`; também não executa varredura de superfície. Para evitar conflito com a ferramenta do sistema, inicie o executável deste projeto pelo caminho completo.

## Executar

A GUI requer Windows 10/11 com GPU compatível com DirectX 12 ou Vulkan. Basta abrir o executável: ele abre uma janela, sem console, que acompanha o tema claro ou escuro do sistema. À esquerda ficam os discos, com um ponto de cor para o estado de cada um; à direita, o painel do disco selecionado, com a nota em destaque, vida estimada, desgaste, temperatura, tempo ligado, última leitura e, em discos ATA, os atributos SMART brutos. Os discos são redescobertos a cada 15 segundos, com no máximo quatro consultas simultâneas em segundo plano. A TUI abre no terminal e usa o mesmo coletor e cálculo de saúde. Use ←/→ para escolher um disco, ↑/↓ para rolar e q ou Esc para sair. A opção `--fixture` imprime uma leitura de exemplo e encerra antes de iniciar a coleta. O runtime não requer Rust, mise ou acesso à rede.

## Pontuação e limites

- A nota de 0 a 100 é determinística, mas não representa probabilidade de falha nem certificação do disco. Com evidência utilizável, notas acima de 50 são classificadas como saudáveis e notas até 50 como atenção; sem evidência utilizável, o estado é **Indisponível** e nenhuma nota é exibida.
- Para NVMe, a heurística considera percentual de desgaste usado, reserva disponível e avisos críticos reportados pelo dispositivo; condições críticas aplicam limites máximos à nota. Não há padrão portátil confiável para interpretar universalmente atributos ATA brutos: eles podem ser exibidos, mas não afetam a nota sem mapeamento específico e verificado por modelo.
- A ETA é uma extrapolação indicativa até 100% de desgaste usado, baseada no segmento atual de histórico desde a leitura mais antiga após o último reset/regressão do contador até a mais recente. Exige aumento positivo observado ao longo do tempo; leituras iguais, regressivas ou dados insuficientes não estabelecem tendência. HDDs não recebem previsão. A ETA não prevê falhas nem garante vida útil.
- O histórico fica em `%APPDATA%\chkdsk\historico.json` (ou na pasta temporária do usuário quando `%APPDATA%` não está definido). Mantém até 35 dias por disco: amostras por hora nos sete dias mais recentes e por dia antes disso. Uma regressão do contador de desgaste descarta o histórico anterior daquele disco para iniciar uma nova linha de base.
- O aplicativo não altera discos e normalmente não precisa de privilégios elevados. Windows, driver, controlador ou adaptador USB podem bloquear consultas SMART; elevar privilégios não torna disponível uma telemetria que o dispositivo ou controlador não fornece.

## Controles

- `←` / `→` ou `Tab` / `Shift+Tab`: alternar entre discos.
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

Tags estáveis seguem exatamente `v{versão}` e não aceitam versão SemVer pré-lançamento. Builds de desenvolvimento usam `v{versão}-dev-{sha7}`. Cada pacote contém seis executáveis independentes com o prefixo `chkdsk-{versão}[-dev-{sha7}]-` e um arquivo `SHA256SUMS.txt`.

| Perfil | Terminal | Janela gráfica | Requisito de CPU |
| --- | --- | --- | --- |
| ARM64 | `tui-windows-arm64.exe` | `gui-windows-arm64.exe` | Windows ARM64, AArch64 com NEON; Rust usa o alvo MSVC ARM64 genérico. |
| x64 | `tui-windows-x64.exe` | `gui-windows-x64.exe` | x86-64-v3 com AVX/AVX2 e suporte do sistema ao estado AVX. Inclui SSE3, SSSE3, SSE4.1/4.2, POPCNT, CMPXCHG16B, F16C, FMA, BMI1/2, LZCNT e MOVBE. |
| x64-baseline | `tui-windows-x64-baseline.exe` | `gui-windows-x64-baseline.exe` | Alvo genérico x86-64 com SSE2; dispensa AVX. Aplicam-se também os requisitos mínimos do Windows instalado. |

Na dúvida sobre a CPU x64, escolha o baseline. Rust usa `-Ctarget-cpu=x86-64-v3` ou `-Ctarget-cpu=x86-64` em compilações separadas. O código C/C++ compilado com MSVC recebe `/arch:AVX2` ou `/arch:SSE2` para o mesmo alvo. Rotinas especializadas de dependências podem selecionar instruções adicionais em tempo de execução pela CPU. Bun executa as tarefas de desenvolvimento; os binários distribuídos são nativos Rust. [Perfis de geração de código do Rust](https://doc.rust-lang.org/rustc/codegen-options/index.html#target-cpu) · [Seleção de ISA no MSVC](https://learn.microsoft.com/en-us/cpp/build/reference/arch-x64)

Para ensaiar o pacote sem publicar, execute `mise exec -- bun scripts/release.ts package --mode dev --sha (git rev-parse HEAD) --output dist/release` no PowerShell. Use um diretório de saída vazio. O empacotamento e a publicação conferem os seis nomes, seus hashes, os cabeçalhos PE, os subsistemas gráfico/console e o perfil de CPU compilado. A CI do PR executa os arquivos empacotados com fixtures em runners Windows x64 e ARM64. Os fluxos dev e estável reutilizam essas verificações antes de publicar.
