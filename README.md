# chkdsk

Painel de saúde de discos para Windows, escrito em Rust, com interface de terminal em português brasileiro (pt_BR). Consulta telemetria SMART/Health, apresenta uma nota heurística conservadora e mantém histórico local para tendências de desgaste. O aplicativo não depende de serviços externos durante a execução.

> Este projeto não é o `chkdsk` do Windows. Não verifica nem repara sistemas de arquivos, não agenda reparos e não substitui `chkdsk.exe`; também não executa varredura de superfície. Para evitar conflito com a ferramenta do sistema, inicie o executável deste projeto pelo caminho completo.

## Executar

Requer Windows 10/11 e um terminal compatível com TUI. Releases incluem executáveis para Windows x64, ARM64 e x86. O runtime não requer Rust, mise ou acesso à rede. A interface usa a paleta Moon do [TokyoNight.nvim v4.14.1](https://github.com/folke/tokyonight.nvim/releases/tag/v4.14.1), fixada no commit `545d72c`: resumo de saúde em primeiro plano, detalhes técnicos em uma seção secundária, avisos temporários com uma central acessível e abas horizontais por disco. Os botões são planos, preenchidos por cor e sem colchetes; mouse e teclado oferecem os mesmos caminhos de navegação. Peso e itálico usam atributos ANSI quando o terminal os suporta; cores e espaçamento também mantêm a hierarquia visual.

## Pontuação e limites

- O índice de saúde de 0 a 100 é determinístico e heurístico: não é probabilidade de falha, certificação, medição de desgaste nem previsão de vida restante, e não é equivalente ao HD Sentinel. `Pass` pode incluir sinais degradados e avisos; `Fail` requer evidência crítica declarada pelo dispositivo/limiar pré-falha; `Unknown` significa que não há evidência utilizável suficiente para calcular uma porcentagem. A interface mostra `N/A` para `Unknown`, nunca o zero interno como `0%`.
- Para ATA/SATA, o aplicativo consulta SMART READ DATA e tenta ler SMART READ THRESHOLDS; usa pass-through ATA nativo, com fallback para o IOCTL SMART legado quando necessário. Os atributos são retidos com valores normalizados atuais/piores e valores brutos. Um limiar normalizado atingido limita o índice a 20; só classifica falha quando a flag pré-falha (bit 0) está marcada ou o dispositivo relata falha global. Ficar até 10 pontos normalizados acima do limiar limita o índice a 70 e gera aviso, sem declarar falha. Um status global SMART de “sem falha”, sozinho, não produz porcentagem de saúde.
- Para HDD, há um índice de contadores brutos somente para os IDs ATA comuns 5 (setores realocados), 10 (tentativas de giro), 187 (erros incorrigíveis reportados), 197 (setores pendentes) e 198 (setores incorrigíveis offline). O ID 5 só é usado quando a codificação raw16 é inequívoca; no perfil WDC WD10EZEX, os dados de temperatura e horas também têm decodificação específica. Raw de outros IDs, perfis de modelo desconhecido e codificações ambíguas não são adivinhados; se uma codificação necessária for ambígua, o índice geral do HDD fica `Unknown`/`N/A` (salvo se outra evidência crítica independente já determinar `Fail`), embora os avisos interpretáveis continuem disponíveis.
- O índice HDD aplica, por contador não zero, uma dedução heurística `base + coeficiente × floor(log10(contagem))`, limitada pelo teto abaixo; contagem zero não deduz pontos. As deduções se combinam multiplicativamente, isto é, `índice × (100 − dedução)/100` para cada classe:

  | ID ATA | Sinal | Base | Coeficiente | Teto |
  | --- | --- | ---: | ---: | ---: |
  | 5 | Setores realocados | 4 | 3 | 25 |
  | 10 | Tentativas de giro | 10 | 4 | 35 |
  | 187 | Erros incorrigíveis reportados | 20 | 8 | 55 |
  | 197 | Setores pendentes | 25 | 10 | 65 |
  | 198 | Setores incorrigíveis offline | 30 | 10 | 70 |

  Esses pesos são escolhas heurísticas do projeto, não calibração empírica nem probabilidade. Contadores como 187, 197 e 198 podem representar sinais relacionados; a combinação pode penalizar sinais sobrepostos. Contador não zero reduz o índice e gera aviso, mas, isoladamente, não prova falha física ou falha declarada pelo firmware.
- Em SSD, o desgaste usado nativo do NVMe e o atributo ATA SMART 202 apenas nos perfis exatos suportados de Crucial MX300, MX500 e BX500 limitam o índice a `100 − desgaste usado`. O perfil ATA interpreta o raw completo de 48 bits como percentual usado e limita valores acima de 100 a 100%. Assim, 100% de desgaste pode resultar em nota zero e aviso sem, por si só, classificar o disco como falho; nota baixa ou contador histórico isolado também não significa falha física. Não há mapeamento linear verificado para o atributo Samsung 177, nem inferência de vida útil a partir de outros IDs brutos.
- O resultado também inclui motivos de aviso estruturados. No NVMe, `media_errors` acima de zero (erros históricos de mídia/integridade de dados) limita a nota a 70 e aplica dedução determinística por ordem de grandeza decimal, limitada a 30 pontos; o bit 5, de região de memória persistente (PMR), gera aviso e limita a nota a 70. Esses avisos, isoladamente, não classificam o disco como falho.
- Temperatura e horas de funcionamento só aparecem quando decodificadas por layouts suportados: o log de saúde NVMe e o perfil estreito WD10EZEX (ID 194 para temperatura e ID 9 para horas, com raw compatível). Outros atributos ATA, IDs duplicados ou codificações desconhecidas não são adivinhados e permanecem indisponíveis.
- A ETA de desgaste é uma extrapolação indicativa até 100% de desgaste usado, baseada no segmento atual de histórico desde a leitura mais antiga após o último reset/regressão do contador até a mais recente. Exige aumento positivo observado ao longo do tempo; leituras iguais, regressivas ou dados insuficientes não estabelecem tendência. HDDs não recebem previsão. A ETA não prevê falhas nem garante vida útil.
- O histórico fica em `%APPDATA%\chkdsk\historico.json` (ou na pasta temporária do usuário quando `%APPDATA%` não está definido). Mantém até 35 dias por disco: amostras por hora nos sete dias mais recentes e por dia antes disso. Uma regressão do contador de desgaste descarta o histórico anterior daquele disco para iniciar uma nova linha de base.
- Na inicialização normal, a partir da versão 0.0.2, o aplicativo solicita elevação pelo UAC nativo do Windows e se reinicia com privilégios de administrador após a confirmação. `chkdsk.exe --version` apenas imprime a versão e não solicita elevação. Windows, driver, controlador ou adaptador USB ainda podem bloquear consultas SMART; elevar privilégios não torna disponível uma telemetria que o dispositivo ou controlador não fornece.

## Interface e controles

A tela principal destaca o percentual de saúde em ASCII e a ETA, visualmente secundária. Os nomes dos discos ficam nas abas permanentes. `N/A` indica que não há evidência suficiente para uma nota; `ETA —` indica ausência de previsão. Confiança da leitura, motivos de indisponibilidade, limites da heurística e demais métricas ficam nos painéis.

- `←` / `→` ou `Tab` / `Shift+Tab`: alternar entre discos; também é possível clicar nas abas ou nos botões de navegação. A roda sobre as abas alterna discos.
- `d` ou o botão `Detalhes`/`D`: abrir e fechar os detalhes do disco selecionado.
- `n` ou o botão `Avisos`/`!`: abrir e fechar a central de notificações. O aviso temporário também pode ser clicado para abrir a central.
- Os painéis direitos começam fechados, são opacos e mutuamente exclusivos. Ao abrir um, o botão do outro fica oculto; as teclas `d` e `n` permitem trocar entre eles. Em terminais estreitos, o painel cobre toda a área abaixo das abas.
- `↑` / `↓` ou `j` / `k`: rolar o painel aberto; `PageUp` / `PageDown` percorrem cinco linhas. A roda dentro do painel e seus botões `↑` / `↓` oferecem o mesmo controle com mouse. Cliques e rolagem dentro dele não atingem controles cobertos.
- `Esc`: fechar o painel aberto; com ambos fechados, sair. `q` ou o botão `Sair`/`×`: sair.
- O inventário e a telemetria são atualizados automaticamente a cada 15 segundos. A seleção e o histórico permanecem vinculados à identidade do disco. Discos com dados parciais continuam visíveis.
- A central mostra os avisos da coleta atual, sem acumular mensagens resolvidas de coletas anteriores. Um problema persistente continua acessível após a nova leitura, sem repetir a notificação temporária a cada atualização. O painel aberto permanece aberto durante a atualização.
- Botões e abas têm texto negrito centralizado e padding de uma célula. Abaixo de 11 linhas usam uma linha compacta; abaixo de 17 linhas o rodapé fica oculto. A nota usa texto compacto quando o ASCII grande não cabe.

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

`validation` confere formatação, regras de release, linkers MSVC, Clippy, testes x64 e compilação nas três arquiteturas. `setup-hook` instala o hook local do `prek`, que verifica `rustfmt`. `release` gera builds otimizados; também existem `release-x64`, `release-arm64` e `release-x86`.

## Releases

Tags estáveis seguem exatamente `v{versão}` e não aceitam versão SemVer pré-lançamento. Para a versão 0.0.2, a tag estável é `v0.0.2` e builds de desenvolvimento usam `v0.0.2-dev-{sha7}` (os sete primeiros caracteres hexadecimais do commit, em minúsculas). Os executáveis têm estes nomes; o sufixo de desenvolvimento aparece somente nesses builds:

```text
chkdsk-0.0.2[-dev-{sha7}]-windows-x64.exe
chkdsk-0.0.2[-dev-{sha7}]-windows-arm64.exe
chkdsk-0.0.2[-dev-{sha7}]-windows-x86.exe
```

`mise run release-check` valida localmente regras SemVer, nomes dos artefatos e mapeamentos de arquitetura sem publicar.
