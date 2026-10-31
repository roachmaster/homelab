use forge_command::{
    validate_descriptors, ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand,
    DescriptorError, Presence,
};

const PATH: ArgumentDescriptor = ArgumentDescriptor {
    name: "path",
    help: "Path to inspect.",
    data_type: DataType::Path,
    presence: Presence::Required,
};

const INSPECT: CommandDescriptor = CommandDescriptor {
    name: "files.inspect",
    help: "Inspect one file.",
    arguments: &[PATH],
};

const STATUS: CommandDescriptor = CommandDescriptor {
    name: "files.status",
    help: "Report capability status.",
    arguments: &[],
};

struct NonTaskCatalog;

impl DescribeCommand for NonTaskCatalog {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[INSPECT, STATUS]
    }
}

#[test]
fn description_is_available_without_task_or_execution_contract() {
    let provider: &dyn DescribeCommand = &NonTaskCatalog;
    let descriptors = provider.descriptors();
    assert_eq!(descriptors, &[INSPECT, STATUS]);
    assert_eq!(validate_descriptors(descriptors), Ok(()));
    assert_eq!(descriptors[0].arguments, &[PATH]);
    assert!(descriptors[1].arguments.is_empty());
}

#[test]
fn each_operation_has_its_own_arguments_and_may_reuse_names() {
    let descriptors = [
        INSPECT,
        CommandDescriptor {
            name: "files.remove",
            help: "Remove the selected file.",
            arguments: &[PATH],
        },
    ];
    assert_eq!(validate_descriptors(&descriptors), Ok(()));
    assert_eq!(descriptors[0].arguments[0].name, "path");
    assert_eq!(descriptors[1].arguments[0].name, "path");
}

#[test]
fn all_semantic_types_have_readable_display_labels() {
    let cases = [
        (DataType::Bool, "bool"),
        (DataType::Text, "text"),
        (DataType::Path, "path"),
        (DataType::I64, "i64"),
        (DataType::U64, "u64"),
        (DataType::F64, "f64"),
        (DataType::Custom("revision-id"), "revision-id"),
    ];
    for (data_type, label) in cases {
        assert_eq!(data_type.to_string(), label);
    }
}

#[test]
fn required_optional_and_display_defaults_are_preserved_without_parsing() {
    const ARGUMENTS: &[ArgumentDescriptor] = &[
        PATH,
        ArgumentDescriptor {
            name: "label",
            help: "An optional label, with no advertised default.",
            data_type: DataType::Text,
            presence: Presence::Optional { default: None },
        },
        ArgumentDescriptor {
            name: "prefix",
            help: "An optional prefix, defaulting to empty text.",
            data_type: DataType::Text,
            presence: Presence::Optional { default: Some("") },
        },
        ArgumentDescriptor {
            name: "limit",
            help: "An optional limit, determined by capability policy.",
            data_type: DataType::U64,
            presence: Presence::Optional {
                default: Some("capability-selected limit"),
            },
        },
        ArgumentDescriptor {
            name: "revision",
            help: "An optional capability-owned revision identifier.",
            data_type: DataType::Custom("revision-id"),
            presence: Presence::Optional { default: None },
        },
    ];
    let descriptor = CommandDescriptor {
        arguments: ARGUMENTS,
        ..INSPECT
    };
    assert_eq!(descriptor.validate(), Ok(()));
    assert_eq!(descriptor.arguments[0].presence, Presence::Required);
    assert_eq!(
        descriptor.arguments[1].presence,
        Presence::Optional { default: None }
    );
    assert_eq!(
        descriptor.arguments[2].presence,
        Presence::Optional { default: Some("") }
    );
    assert_eq!(
        descriptor.arguments[3].presence,
        Presence::Optional {
            default: Some("capability-selected limit")
        }
    );
}

#[test]
fn operation_without_arguments_is_valid_but_empty_catalog_is_not() {
    assert_eq!(STATUS.validate(), Ok(()));
    assert_eq!(validate_descriptors(&[STATUS]), Ok(()));
    assert_eq!(
        validate_descriptors(&[]),
        Err(DescriptorError::EmptyDescriptorSet)
    );
}

#[test]
fn blank_operation_names_and_help_are_rejected() {
    for blank in ["", " ", "\t\n", "\u{2003}"] {
        assert_eq!(
            CommandDescriptor {
                name: blank,
                ..INSPECT
            }
            .validate(),
            Err(DescriptorError::EmptyCommandName)
        );
        assert_eq!(
            CommandDescriptor {
                help: blank,
                ..INSPECT
            }
            .validate(),
            Err(DescriptorError::EmptyCommandHelp {
                command: "files.inspect"
            })
        );
    }
}

#[test]
fn empty_and_whitespace_argument_names_are_rejected_with_position() {
    for arguments in [
        &[PATH, ArgumentDescriptor { name: "", ..PATH }][..],
        &[
            PATH,
            ArgumentDescriptor {
                name: "\u{2003}\t",
                ..PATH
            },
        ][..],
    ] {
        assert_eq!(
            CommandDescriptor {
                arguments,
                ..INSPECT
            }
            .validate(),
            Err(DescriptorError::EmptyArgumentName {
                command: "files.inspect",
                index: 1
            })
        );
    }
}

#[test]
fn empty_and_whitespace_argument_help_is_rejected_with_context() {
    for arguments in [
        &[ArgumentDescriptor { help: "", ..PATH }][..],
        &[ArgumentDescriptor {
            help: "\n \u{2003}",
            ..PATH
        }][..],
    ] {
        assert_eq!(
            CommandDescriptor {
                arguments,
                ..INSPECT
            }
            .validate(),
            Err(DescriptorError::EmptyArgumentHelp {
                command: "files.inspect",
                argument: "path"
            })
        );
    }
}

#[test]
fn empty_and_whitespace_custom_type_names_are_rejected() {
    for arguments in [
        &[ArgumentDescriptor {
            data_type: DataType::Custom(""),
            ..PATH
        }][..],
        &[ArgumentDescriptor {
            data_type: DataType::Custom("\u{2003}\t"),
            ..PATH
        }][..],
    ] {
        assert_eq!(
            CommandDescriptor {
                arguments,
                ..INSPECT
            }
            .validate(),
            Err(DescriptorError::EmptyCustomDataType {
                command: "files.inspect",
                argument: "path"
            })
        );
    }
}

#[test]
fn duplicate_argument_name_is_rejected_even_when_other_metadata_differs() {
    let descriptor = CommandDescriptor {
        arguments: &[
            PATH,
            ArgumentDescriptor {
                help: "A conflicting argument using the same name.",
                data_type: DataType::Text,
                presence: Presence::Optional { default: None },
                ..PATH
            },
        ],
        ..INSPECT
    };
    assert_eq!(
        descriptor.validate(),
        Err(DescriptorError::DuplicateArgumentName {
            command: "files.inspect",
            argument: "path"
        })
    );
}

#[test]
fn duplicate_operation_name_is_rejected_even_when_other_metadata_differs() {
    assert_eq!(
        validate_descriptors(&[
            INSPECT,
            CommandDescriptor {
                name: INSPECT.name,
                ..STATUS
            }
        ]),
        Err(DescriptorError::DuplicateCommandName {
            command: "files.inspect"
        })
    );
}

#[test]
fn names_are_exact_case_sensitive_labels_not_a_cli_grammar() {
    assert_eq!(
        validate_descriptors(&[
            INSPECT,
            CommandDescriptor {
                name: "Files.Inspect",
                arguments: &[
                    PATH,
                    ArgumentDescriptor {
                        name: "Path",
                        ..PATH
                    },
                    ArgumentDescriptor {
                        name: "path label",
                        ..PATH
                    }
                ],
                ..INSPECT
            }
        ]),
        Ok(())
    );
}

#[test]
fn set_validation_checks_each_member_and_returns_the_first_error() {
    assert_eq!(
        validate_descriptors(&[
            STATUS,
            CommandDescriptor {
                arguments: &[ArgumentDescriptor { help: "", ..PATH }],
                ..INSPECT
            },
            CommandDescriptor { name: "", ..STATUS }
        ]),
        Err(DescriptorError::EmptyArgumentHelp {
            command: "files.inspect",
            argument: "path"
        })
    );
}

#[test]
fn errors_implement_standard_error_and_provide_actionable_context() {
    let cases = [
        (
            DescriptorError::EmptyDescriptorSet,
            "descriptor set is empty",
        ),
        (DescriptorError::EmptyCommandName, "command name is blank"),
        (
            DescriptorError::EmptyCommandHelp { command: "op" },
            "command \"op\" help is blank",
        ),
        (
            DescriptorError::DuplicateCommandName { command: "op" },
            "duplicate command name \"op\"",
        ),
        (
            DescriptorError::EmptyArgumentName {
                command: "op",
                index: 2,
            },
            "command \"op\" argument 2 name is blank",
        ),
        (
            DescriptorError::EmptyArgumentHelp {
                command: "op",
                argument: "input",
            },
            "command \"op\" argument \"input\" help is blank",
        ),
        (
            DescriptorError::EmptyCustomDataType {
                command: "op",
                argument: "input",
            },
            "command \"op\" argument \"input\" custom type name is blank",
        ),
        (
            DescriptorError::DuplicateArgumentName {
                command: "op",
                argument: "input",
            },
            "command \"op\" has duplicate argument name \"input\"",
        ),
    ];
    for (error, message) in cases {
        let error: &dyn std::error::Error = &error;
        assert!(error.to_string().contains(message));
        assert!(error.source().is_none());
    }
}
