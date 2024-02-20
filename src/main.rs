use async_openai::types::Role;
use async_openai::{config::OpenAIConfig, Client};
use clap::Parser;
use eyre::{Context, Result};
use indoc::formatdoc;
use sqlx::Column;
use sqlx::ConnectOptions;
use sqlx::Executor;
use sqlx::Row;
use std::str::FromStr;

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    verbose: bool,

    #[clap(long, default_value = "courses.sqlite3")]
    db: String,

    #[clap(long, default_value = "gpt-4-turbo-preview")]
    model: String,

    /// Question to answer based on the course database.
    question: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Open a read-only sqlite connection
    let mut conn = sqlx::sqlite::SqliteConnectOptions::from_str(&args.db)
        .wrap_err("Invalid db connection string")?
        .read_only(true)
        .connect()
        .await?;

    // Make an OpenAI client.
    let oai_config = OpenAIConfig::default();
    let oai_client = Client::with_config(oai_config);

    let mut prompt: Vec<(Role, String)> = vec![];

    prompt.push((
        Role::System,
        formatdoc! {r#"
            You are an agent designed to help students with course registration at Georgia Tech. You have access to a SQLite database of available sections to register. Your job is to write a query against that database to answer a student's question about course registration. You should be very selective about the columns you select from the database---only include important information to answer the question. Always include a CRN, if it makes sense to do so. Do NOT include enrollment information if the user doesn't ask for it.

            Assume this student is enrolled in the Atlanta campus, and only interested in courses they can register for in-person.

            If a student refers to a course like 'CS 1331', they are referring to the course number, '1331' and subject 'CS'. If a student refers to 'CS 8803 ANI', they're refering to the 'ANI' section of CS 8803.

            Here is the schema of the database:
            ```sql
            {DB_INFO_PROMPT}
            ```

            The next message will have a question from a student. Read it carefully:
        "#},
    ));

    prompt.push((Role::User, args.question.clone()));

    prompt.push((
        Role::System,
        "Given the following question, write a single SQL query to answer it. Take a deep breath and think carefully before responding. Respond ONLY with the text of the SQL query, or else it won't work and the student will be very sad.".to_string(),
    ));

    // Build the OpenAI request.
    let chat_completion_request = async_openai::types::CreateChatCompletionRequest {
        model: args.model.to_string(),
        messages: prompt
            .into_iter()
            .map(
                |(role, content)| async_openai::types::ChatCompletionRequestMessage {
                    role,
                    content: Some(content),
                    ..Default::default()
                },
            )
            .collect(),
        ..Default::default()
    };

    let response = oai_client
        .chat()
        .create(chat_completion_request)
        .await
        .wrap_err("Failed to open result stream from OpenAI")?;

    // Get the query from the response text.
    let response_text = response.choices[0].message.content.as_ref().unwrap().trim();

    // Strip lines starting with "```"
    let response_text = response_text
        .lines()
        .filter(|line| !line.trim().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");

    if args.verbose {
        eprintln!("{response_text}");
    }

    // Run the SQL.
    let rows = conn
        .fetch_all(response_text.as_str())
        .await
        .wrap_err("Failed to execute SQL query")?;

    let mut table = term_table::Table::new();
    table.style = term_table::TableStyle::rounded();
    table.separate_rows = true;

    let mut header_row = vec![];

    // Print the results.
    for row in rows {
        let mut tab_row = vec![];
        header_row.clear();
        for column in row.columns() {
            if column.name() == "raw" {
                continue;
            }

            header_row.push(column.name().to_string());

            let string_val = row
                .try_get::<String, _>(column.name())
                .or_else(|_| row.try_get::<i64, _>(column.name()).map(|x| x.to_string()))?;

            tab_row.push(string_val);
        }

        let mut row = term_table::row::Row::new(tab_row);
        row.has_separator = false;
        table.rows.push(row);
    }

    // insert the header row first
    table.rows.insert(0, term_table::row::Row::new(header_row));

    // Second row has a separator (if we have a second row)
    if table.rows.len() > 1 {
        table.rows[1].has_separator = true;
    }

    // Print the table.
    println!("{}", table.render());

    Ok(())
}

const DB_INFO_PROMPT: &str = r#"

-- Course sections
CREATE TABLE sections (
	id text not null primary key,
	term text not null, -- Course term, like 202402 for Spring 2024
	term_description text not null, -- Human-readable term description, like 'Spring 2024'
	crn text not null, -- Course Registration Number. Unique to each section, and necessary to register.
	number text not null, -- Course number, like '9000' in 'PHYS 9000'
	subject text not null, -- Course subject, like 'PHYS' in 'PHYS 9000'
	subject_description text not null, -- Human-readable subject description, like 'Physics'

	section text not null, -- Section number, like 'A' or 'O'. Note that an 'O' section actually indicates an online course, even if the campus is 'Georgia Tech-Atlanta *'.

	campus text not null, -- What campus. Note that in-person students can't register for 'Online' courses. One of:
    --  'Georgia Tech-Atlanta *'
    --  'GT Lorraine-Undergrad Programs'
    --  'Foreign Exchange'
    --  'GT Lorraine-Graduate Programs'
    --  'Georgia Tech - Shenzhen'
    --  'Video'
    --  'MBA Evening Program'
    --  'Online'
    --  'Georgia Tech Studies Abroad'
    --  'Graduate Certificate'
    --  'Georgia Southern/GTREP'
    --  'GT, Peking University, &amp; Emory'
    --  'Georgia Tech-Savannah'
    --  'Georgia Tech - Korea'
    --  'Global'

	schedule_type text not null, -- One of:
    --  'Lecture*'
    --  'Dissertation*'
    --  'Directed Study*'
    --  'Thesis*'
    --  'Seminar*'
    --  'Supervised Laboratory*'
    --  'Studio*'
    --  'Internship/Practicum*'
    --  'Unsupervised Laboratory*'
    --  'Breakout*'
    --  'Mixed Laboratory*'
    --  'Recitation*'
    --  'Co-op Work Assignment'
    --  'Practice Teaching*'
    --  'Common Exam*'
    
	course_title not null, -- The title of the course.
	credit_hours integer not null, -- The number of credit hours awarded for taking this section.
	
    -- Information about who is enrolled in the class:
    max_enrollment integer not null,
	enrollment integer not null,
	seats_available integer not null,
	waitlist_capacity integer not null,
	waitlist_count integer not null,
	waitlist_available integer not null,

	open string not null, -- Whether the section is open for registration. 'true' or 'false'.
	attributes text, -- Comma-separated list of attributes, like 'ETHS,HUM' for a course that fulfills the ethics and humanities requirement.
	raw json not null -- Raw payload from registration system (can be ignored)
);

-- Names and contact informaiton of faculty.
CREATE TABLE faculty (
	id text not null primary key,
	name text not null,
	email text not null
);

-- The faculty teaching each section.
CREATE TABLE course_faculty (
	course_id text not null references sections(id),
	faculty_id text not null references faculty(id),
	primary key (course_id, faculty_id)
);

"#;
