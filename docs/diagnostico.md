# Diagnóstico do disksonar

A indicação de saúde combina o veredito do dispositivo com evidências disponíveis, como desgaste, reserva e alertas NVMe. O percentual em destaque, quando aparece, indica resistência nominal restante. Ele não é uma probabilidade de falha. Desgaste informado de zero é uma medição válida. Valores ausentes aparecem como indisponíveis.

Para NVMe, a fonte preferencial é Percentage Used. Quando faltam informações, a leitura também consulta os contadores de confiabilidade do Windows para o mesmo caminho de disco. Os dados ATA brutos ficam em **Informações avançadas**. A ficha pública do MACROVIP MV240GB não documenta as unidades dos atributos SMART por firmware, por isso o disksonar mantém valores como o do ID `09` em formato bruto. [Ficha do fabricante](https://macrovip.com/en/produto/ssd-macrovip-premium-240gb-2-5-sata-3-mv240gb/)

## Histórico e permissões

A previsão é uma extrapolação até 100% de desgaste usado. Exige pelo menos 24 horas entre leituras e aumento acima de zero de até dez pontos percentuais por dia. A previsão não se aplica a HDDs e não estima quando o disco falhará.

O histórico fica em `%APPDATA%\disksonar\historico.json`. A primeira execução sem arquivo próprio lê `%APPDATA%\chkdsk\historico.json`, preservando o arquivo legado. São guardados até 35 dias de amostras por disco. Uma regressão no contador inicia um novo segmento. Se `APPDATA` estiver ausente, a pasta temporária do usuário é usada para o histórico.

O Windows pode pedir consentimento quando uma leitura requer privilégios adicionais. Se a elevação for cancelada, a janela continua disponível com os dados já obtidos. O suporte a SMART ainda depende do dispositivo, do driver e do adaptador.

## Controles

- Use **←** e **→** para mudar de disco. Use **Tab** para navegar entre os controles e **Enter** ou **Espaço** para acioná-los.
- Role com o mouse ou com **↑**, **↓**, **Page Up**, **Page Down**, **Home** e **End**.
- Em janela estreita, abra o menu **Discos** para acessar a lista e **Configurações**. **Esc** fecha o menu aberto.
- Em **Configurações**, tema aceita Sistema, Claro e Escuro. Animações aceita Sistema, Ativadas e Desativadas. As preferências ficam em `%APPDATA%\disksonar\settings.json`.

## Exportar relatórios

Nos builds da branch `dev` com a função **Exportar**, a barra de título abre o diálogo de salvar do Windows. O JSON reúne a leitura dos discos, o histórico e os atributos ATA. O CSV usa UTF-8 com BOM e protege campos de texto contra fórmulas de planilha. Os identificadores podem constar no JSON quando você decide exportar.
