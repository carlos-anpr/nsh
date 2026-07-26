use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    ReadOnly,
    Modifies,
    Destructive,
}

impl Effect {
    pub fn parse(s: &str) -> Effect {
        match s {
            "ReadOnly" => Effect::ReadOnly,
            "Modifies" => Effect::Modifies,
            // Ante cualquier cosa rara, lo mas restrictivo. Nunca al reves.
            _ => Effect::Destructive,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlannedCommand {
    pub explanation: String,
    pub command: String,
    pub expected_effect: Effect,
}

#[derive(Debug, Clone)]
pub enum PlanOutcome {
    Command(PlannedCommand),
    DirectText(String),
}

pub struct ShellContext {
    pub cwd: PathBuf,
    pub os: String,
    /// Ultimos comandos con su codigo de salida. Sin esto el LLM no puede
    /// corregirse tras un fallo, que es la mitad del valor de la herramienta.
    pub recent: Vec<(String, i32)>,
    /// Salida del ultimo comando, recortada. Solo se manda en /fix y /why.
    pub last_output: Option<String>,
    /// Nombres REALES del cwd. Sin esto el modelo se inventa las mayusculas.
    pub entries: Vec<String>,
    /// Rutas que el usuario referencio con @ y que nsh YA ha verificado.
    pub referenced: Vec<String>,
}

pub trait Planner {
    fn plan(&self, request: &str, ctx: &ShellContext) -> Result<PlanOutcome>;
    fn explain(&self, question: &str, ctx: &ShellContext) -> Result<String>;
}

pub mod anthropic;
