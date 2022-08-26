use std::{
    collections::HashMap,
    fmt, io, panic,
    time::{Duration, Instant},
};

use anyhow::Result;
use aws_sdk_dynamodb::{Client, Endpoint};
use aws_sdk_s3::model::server_side_encryption_configuration;
use clap::{Args, Parser, Subcommand};
use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use http::Uri;
use tokio::sync::mpsc::{Receiver, Sender};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Row, Table, TableState},
    Frame, Terminal,
};

#[derive(Debug, Parser)] // requires `derive` feature
#[clap(name = "nebulous")]
#[clap(about = "A fictional versioning CLI", long_about = None, author = "Jonathan Rothberg")]
struct Cli {
    #[clap(long)]
    endpoint: Option<String>,
    #[clap(subcommand)]
    subcmd: Option<SubCmd>,
}

#[derive(Debug, Subcommand)]
enum SubCmd {
    UI,
    QueryDynamo(QueryDynamoArgs),
}

#[derive(Debug, Args)]
struct QueryDynamoArgs {
    #[clap(long)]
    query: Option<String>,
    #[clap(long = "table-name")]
    table_name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    match run(&args).await {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("Error in main: {:?}", e);
            Err(e)
        }
    }
}

async fn run(args: &Cli) -> Result<()> {
    match &args.subcmd {
        Some(SubCmd::UI) => run_ui(args.endpoint.as_deref()).await?,
        Some(SubCmd::QueryDynamo(a)) => query(a, args.endpoint.as_deref()).await?,
        None => {}
    }
    Ok(())
}

async fn query(args: &QueryDynamoArgs, endpoint: Option<&str>) -> Result<()> {
    println!("Querying dymamo: {:?}", args.table_name);
    let ep = endpoint.unwrap_or("").to_string();
    let client = DynamoClient::new(&ep).await;
    let output = client
        .client()
        .scan()
        .table_name(&args.table_name)
        .send()
        .await?;
    println!("{:#?}", output);
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Event<I> {
    Input(I),
    Tick,
}

async fn run_ui(endpoint: Option<&str>) -> Result<()> {
    enable_raw_mode()?;

    panic::set_hook(Box::new(|info| {
        println!("Panic: {}", info);
        disable_raw_mode().expect("restore terminal raw mode");
    }));

    let mut rx = start_key_events();
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    io::stdout().execute(EnterAlternateScreen)?;
    terminal.clear()?;

    // let (ui_tx, ui_rx): (Sender<Event<KeyEvent>>, Receiver<Event<KeyEvent>>) =
    let (mut ui_tx, mut ui_rx) = tokio::sync::mpsc::channel(1);

    let ep = endpoint.unwrap_or("").to_string();
    let mut app = App::new(ui_tx.clone(), &ep).await;

    let client = DynamoClient::new(&ep).await;
    let _ = refresh_table_list(client, ui_tx.clone());

    loop {
        terminal.draw(|rect| {
            let _ = draw(rect, &mut app);
        })?;

        tokio::select! {
        Some(event) = rx.recv() => {
            match event {
                Event::Input(event) =>
                    match event.code {
                        KeyCode::Char('q') => {
                            disable_raw_mode()?;
                                        io::stdout().execute(LeaveAlternateScreen)?;
                                        terminal.show_cursor()?;
                            break;
                    },

                    KeyCode::Char('j') => {
                        let _ = app.move_down();
                    },
                    KeyCode::Char('k') => {
                        let _ = app.move_up();
                    },
                    KeyCode::Enter => {
                        app.table_selected().await?;
                    },
                    KeyCode::Tab => {
                        let _ = app.toggle_active_pane();
                    },
                        _ => {}

                    }

                Event::Tick => {}
            }
        }
            Some(ui_event) = ui_rx.recv() => {
                match ui_event {
                    UIEvent::RefreshDynamoTableList(tables) => app.load_tables(&tables),
                    UIEvent::LoadTable(name) => load_table(&ep, &name,  ui_tx.clone()).await?,
                    UIEvent::DisplayTable(table) => app.select_table(table)
                }
            }
        }
    }

    Ok(())
}

enum ActiveView {
    None,
    TableList,
    TableData,
}

struct App {
    dynamo_client: DynamoClient,
    endpoint: String,
    tables: ListState,
    table_list: Vec<String>,
    items: TableState,
    active: ActiveView,
    io_tx: Option<Sender<UIEvent>>,
    selected_table: NebTable,
}

impl App {
    pub async fn new(io_tx: Sender<UIEvent>, endpoint: &str) -> Self {
        let mut table_list_state = ListState::default();
        table_list_state.select(Some(0));

        let mut table_data_state = TableState::default();
        table_data_state.select(None);

        Self {
            io_tx: Some(io_tx),
            dynamo_client: DynamoClient::new(endpoint).await,
            endpoint: String::new(),
            tables: table_list_state,
            items: table_data_state,
            table_list: vec![],
            active: ActiveView::TableList,
            selected_table: NebTable::default(),
        }
    }

    pub fn load_tables(&mut self, tables: &[String]) {
        self.table_list = tables.to_vec();
    }

    pub fn move_down(&mut self) -> Result<()> {
        match self.active {
            ActiveView::TableList => {
                if let Some(selected) = self.tables.selected() {
                    if selected >= self.table_list.len() {
                        self.tables.select(Some(0))
                    } else {
                        self.tables.select(Some(selected + 1))
                    }
                }
            }
            ActiveView::TableData => {
                if let Some(selected) = self.items.selected() {
                    if selected >= self.selected_table.rows.len() {
                        self.items.select(Some(1))
                    } else {
                        self.items.select(Some(selected + 1))
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn move_up(&mut self) -> Result<()> {
        match self.active {
            ActiveView::TableList => {
                if let Some(selected) = self.tables.selected() {
                    if selected > 0 {
                        self.tables.select(Some(selected - 1))
                    } else {
                        self.tables.select(Some(self.table_list.len() - 1))
                    }
                }
            }
            ActiveView::TableData => {
                if let Some(selected) = self.items.selected() {
                    if selected > 0 {
                        self.items.select(Some(selected - 1))
                    } else {
                        self.items.select(Some(self.selected_table.rows.len() - 1))
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn table_selected(&mut self) -> Result<()> {
        if let Some(selected) = self.tables.selected() {
            if selected < self.table_list.len() {
                let name = &self.table_list[selected];
                self.dispatch(UIEvent::LoadTable(name.clone())).await?;
            }
        }
        Ok(())
    }

    pub fn select_table(&mut self, table: NebTable) {
        self.selected_table = table;
    }

    async fn dispatch(&self, action: UIEvent) -> Result<()> {
        if let Some(io_tx) = &self.io_tx {
            let _ = io_tx.send(action).await;
        }

        Ok(())
    }

    pub fn toggle_active_pane(&mut self) -> Result<()> {
        match self.active {
            ActiveView::TableList => {
                self.active = ActiveView::TableData;
                if self.items.selected().is_none() {
                    self.items.select(Some(0));
                }
            }
            ActiveView::TableData => self.active = ActiveView::TableList,
            _ => self.active = ActiveView::TableList,
        }

        Ok(())
    }
}

fn refresh_table_list(client: DynamoClient, tx: tokio::sync::mpsc::Sender<UIEvent>) -> Result<()> {
    tokio::spawn(async move {
        match client.client().list_tables().send().await {
            Ok(l) => {
                let tables: Vec<String> = l.table_names().unwrap_or_default().to_vec();
                let _ = tx.send(UIEvent::RefreshDynamoTableList(tables)).await;
            }
            Err(e) => println!("{:?}", e),
        }
    });

    Ok(())
}

#[derive(Debug, Clone)]
enum ItemValue {
    Null,
    String(String),
    Number(i64),
    Bool(bool),
    Map(HashMap<String, ItemValue>),
    List(Vec<ItemValue>),
}

impl fmt::Display for ItemValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemValue::Null => write!(f, "null"),
            ItemValue::String(s) => write!(f, "{}", s),
            ItemValue::Number(n) => write!(f, "{}", n),
            ItemValue::Bool(b) => write!(f, "{}", b),
            ItemValue::Map(_m) => write!(f, "Map..."),
            ItemValue::List(_l) => write!(f, "List"),
        }
    }
}

impl From<aws_sdk_dynamodb::model::AttributeValue> for ItemValue {
    fn from(item: aws_sdk_dynamodb::model::AttributeValue) -> Self {
        match item {
            aws_sdk_dynamodb::model::AttributeValue::Bool(b) => Self::Bool(b),
            aws_sdk_dynamodb::model::AttributeValue::N(n) => {
                Self::Number(n.parse().unwrap_or_default())
            }
            aws_sdk_dynamodb::model::AttributeValue::S(s) => Self::String(s),
            aws_sdk_dynamodb::model::AttributeValue::Null(_) => Self::Null,
            aws_sdk_dynamodb::model::AttributeValue::L(l) => {
                let v: Vec<Self> = l.into_iter().map(|a| a.into()).collect();
                Self::List(v)
            }
            aws_sdk_dynamodb::model::AttributeValue::M(m) => {
                let h: HashMap<String, Self> = m.into_iter().map(|(k, v)| (k, v.into())).collect();
                Self::Map(h)
            }
            _ => panic!("unexpected attribute value: {:?}", item),
        }
    }
}

#[derive(Debug, Clone)]
struct KV {
    key: String,
    value: ItemValue,
}

#[derive(Debug, Clone)]
struct TableRow {
    data: Vec<KV>,
}

impl From<HashMap<String, aws_sdk_dynamodb::model::AttributeValue>> for TableRow {
    fn from(item: HashMap<String, aws_sdk_dynamodb::model::AttributeValue>) -> Self {
        let data = item
            .into_iter()
            .map(|(k, v)| KV {
                key: k,
                value: v.into(),
            })
            .collect();
        Self { data }
    }
}

impl From<&HashMap<String, aws_sdk_dynamodb::model::AttributeValue>> for TableRow {
    fn from(item: &HashMap<String, aws_sdk_dynamodb::model::AttributeValue>) -> Self {
        let data = item
            .iter()
            .map(|(k, v)| KV {
                key: k.to_string(),
                value: v.to_owned().into(),
            })
            .collect();
        Self { data }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NebTable {
    rows: Vec<TableRow>,
    headers: Vec<String>,
}

impl NebTable {
    fn new(headers: Vec<String>, rows: Vec<TableRow>) -> Self {
        Self { headers, rows }
    }
}

async fn load_table(
    endpoint: &str,
    table_name: &str,
    mut tx: tokio::sync::mpsc::Sender<UIEvent>,
) -> Result<()> {
    let client = DynamoClient::new(endpoint).await;

    let items = client
        .client()
        .scan()
        .table_name(table_name)
        .limit(200)
        .send()
        .await?;

    if let Some(items) = items.items() {
        let headers = if let Some(first) = items.first() {
            first.iter().map(|(k, _)| k.clone()).collect()
        } else {
            vec![]
        };

        let vals: Vec<TableRow> = items.into_iter().map(|i| i.into()).collect();
        // println!("{:?}", vals);

        let table = NebTable::new(headers, vals);
        let _ = tx.send(UIEvent::DisplayTable(table)).await;
    }

    Ok(())
}

// impl Default for App {
//     fn default() -> Self {
//         App {
//             dynamo_client: aws_sdk_dynamodb::Client::new(o),
//             endpoint: String::new(),
//             tables: ListState::default(),
//             items: ListState::default(),
//         }
//     }
// }
#[derive(Clone, Debug)]
pub enum UIEvent {
    RefreshDynamoTableList(Vec<String>),
    LoadTable(String),
    DisplayTable(NebTable),
}

fn draw<B: Backend>(f: &mut Frame<B>, app: &mut App) -> Result<()> {
    let chunks = Layout::default()
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)].as_ref())
        .direction(Direction::Horizontal)
        .split(f.size());

    // let tables = app.dynamo_client.client().list_tables().send().await;

    // let tables = List::new(vec![ListItem::new(vec![Spans::from(Span::raw("test"))])])
    //     .block(Block::default().borders(Borders::ALL).title("Tables"))
    //     .highlight_style(Style::default().add_modifier(Modifier::BOLD))
    //     .highlight_symbol("> ");

    let tables: Vec<ListItem> = app
        .table_list
        .iter()
        .map(|t| ListItem::new(vec![Spans::from(Span::from(t.clone()))]))
        .collect();
    let tables = List::new(tables)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Dynamo Tables"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(tables, chunks[0], &mut app.tables);

    let table_items: Vec<ListItem> = app
        .selected_table
        .rows
        .iter()
        .take(3)
        .map(|r| ListItem::new(vec![Spans::from(Span::from(r.data[0].key.clone()))]))
        .collect();
    let items = List::new(table_items)
        .block(Block::default().borders(Borders::ALL).title("Items"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    // let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let selected_style = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let normal_style = Style::default();
    let header_cells = app
        .selected_table
        .headers
        .iter()
        .take(3)
        .map(|h| Cell::from(h.to_string()).style(Style::default().fg(Color::Red)));
    let header = Row::new(header_cells)
        .style(normal_style)
        .height(1)
        .bottom_margin(2);
    let rows = app.selected_table.rows.iter().map(|item| {
        let height = item
            .data
            .iter()
            .map(|content| content.key.chars().filter(|c| *c == '\n').count())
            .max()
            .unwrap_or(0)
            + 1;
        let cells = item.data.iter().map(|c| Cell::from(c.value.to_string()));
        Row::new(cells).height(height as u16).bottom_margin(1)
    });

    let width = if !app.selected_table.headers.is_empty() {
        20
    } else {
        0
    };

    let widths: Vec<Constraint> = app
        .selected_table
        .headers
        .iter()
        .take(3)
        .map(|_h| Constraint::Length(width as u16))
        .collect();
    let t = Table::new(rows)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Table"))
        .highlight_style(selected_style)
        .highlight_symbol(">> ")
        .column_spacing(10)
        .widths(&widths);
    f.render_stateful_widget(t, chunks[1], &mut app.items);

    Ok(())
}

fn start_key_events() -> tokio::sync::mpsc::Receiver<Event<KeyEvent>> {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let tick_rate = Duration::from_millis(200);
    tokio::spawn(async move {
        let mut last_tick = Instant::now();
        loop {
            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if event::poll(timeout).expect("poll works") {
                if let CEvent::Key(key) = event::read().expect("can read events") {
                    let _ = tx.send(Event::Input(key)).await;
                }
            }

            if last_tick.elapsed() >= tick_rate {
                if (tx.send(Event::Tick).await).is_ok() {
                    last_tick = Instant::now();
                }
            }
        }
    });

    rx
}

struct DynamoClient {
    client: aws_sdk_dynamodb::Client,
}

impl DynamoClient {
    pub async fn new(endpoint: &str) -> Self {
        // Select a profile by setting the `AWS_PROFILE` environment variable.
        let config = aws_config::load_from_env().await;
        let mut dynamodb_local_config = aws_sdk_dynamodb::config::Builder::from(&config);
        if !endpoint.is_empty() {
            let uri = endpoint.parse().unwrap();
            dynamodb_local_config =
                dynamodb_local_config.endpoint_resolver(Endpoint::immutable(uri));
        }
        let cfg = dynamodb_local_config.build();

        let client = Client::from_conf(cfg);

        Self { client }
    }

    pub fn client(&self) -> &aws_sdk_dynamodb::Client {
        &self.client
    }
}
