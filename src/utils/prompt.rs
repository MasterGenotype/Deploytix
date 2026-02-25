//! User prompt utilities using dialoguer

use crate::utils::error::{DeploytixError, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Password, Select};

/// Prompt for text input
pub fn prompt_input(prompt: &str, default: Option<&str>) -> Result<String> {
    let theme = ColorfulTheme::default();
    let mut input = Input::with_theme(&theme).with_prompt(prompt);

    if let Some(d) = default {
        input = input.default(d.to_string());
    }

    input
        .interact_text()
        .map_err(|_| DeploytixError::UserCancelled)
}

/// Prompt for password (hidden input)
pub fn prompt_password(prompt: &str, confirm: bool) -> Result<String> {
    let theme = ColorfulTheme::default();
    let mut password = Password::with_theme(&theme).with_prompt(prompt);

    if confirm {
        password = password.with_confirmation("Confirm password", "Passwords do not match");
    }

    password
        .interact()
        .map_err(|_| DeploytixError::UserCancelled)
}

/// Prompt for yes/no confirmation
pub fn prompt_confirm(prompt: &str, default: bool) -> Result<bool> {
    let theme = ColorfulTheme::default();
    Confirm::with_theme(&theme)
        .with_prompt(prompt)
        .default(default)
        .interact_opt()
        .map_err(|e| DeploytixError::Io(std::io::Error::other(e.to_string())))?
        .ok_or(DeploytixError::UserCancelled)
}

/// Prompt for selection from a list
pub fn prompt_select<T: ToString>(prompt: &str, items: &[T], default: usize) -> Result<usize> {
    let theme = ColorfulTheme::default();
    Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(items)
        .default(default)
        .interact_opt()
        .map_err(|e| DeploytixError::Io(std::io::Error::other(e.to_string())))?
        .ok_or(DeploytixError::UserCancelled)
}

/// Prompt for fuzzy selection from a list
#[allow(dead_code)]
pub fn prompt_fuzzy_select<T: ToString>(prompt: &str, items: &[T]) -> Result<usize> {
    let theme = ColorfulTheme::default();
    FuzzySelect::with_theme(&theme)
        .with_prompt(prompt)
        .items(items)
        .interact_opt()
        .map_err(|e| DeploytixError::Io(std::io::Error::other(e.to_string())))?
        .ok_or(DeploytixError::UserCancelled)
}

/// Prompt for optional input (can be empty)
#[allow(dead_code)]
pub fn prompt_optional(prompt: &str) -> Result<Option<String>> {
    let theme = ColorfulTheme::default();
    let input: String = Input::with_theme(&theme)
        .with_prompt(prompt)
        .allow_empty(true)
        .interact_text()
        .map_err(|_| DeploytixError::UserCancelled)?;

    if input.is_empty() {
        Ok(None)
    } else {
        Ok(Some(input))
    }
}

/// Display a warning and ask for confirmation
pub fn warn_confirm(warning: &str) -> Result<bool> {
    println!("\n⚠️  WARNING: {}\n", warning);
    prompt_confirm("Continue?", false)
}

/// Display an error message
#[allow(dead_code)]
pub fn error(message: &str) {
    eprintln!("❌ Error: {}", message);
}

/// Display a success message
#[allow(dead_code)]
pub fn success(message: &str) {
    println!("✓ {}", message);
}

/// Display an info message
#[allow(dead_code)]
pub fn info(message: &str) {
    println!("ℹ {}", message);
}
