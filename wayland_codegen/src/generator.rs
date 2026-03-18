/*
Copyright 2026 Google LLC

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

     https://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

use crate::protocol::*;
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn generate(protocol: &Protocol) -> String {
    let mut parts = Vec::new();
    let mut interface_names = Vec::new();
    let mut handler_trait_names = Vec::new();
    let mut dispatch_request_cases = Vec::new();
    let mut dispatch_event_cases = Vec::new();

    for item in &protocol.items {
        if let ProtocolItem::Interface(interface) = item {
            let name = &interface.name;
            let mod_name = format_ident!("{}", name);
            let handler_trait_name = format_ident!("{}Handler", snake_to_camel(name));

            interface_names.push(name.clone());
            handler_trait_names.push(quote! { #mod_name::#handler_trait_name });

            dispatch_request_cases.push(quote! {
                #name => #mod_name::dispatch_request(msg, handler, ctx),
            });
            dispatch_event_cases.push(quote! {
                #name => #mod_name::dispatch_event(msg, handler, ctx),
            });

            parts.push(generate_interface(interface));
        }
    }

    let protocol_name = format_ident!("{}", protocol.name);
    let delegation_macro = generate_delegation_macro(protocol);

    let expanded = quote! {
        pub mod #protocol_name {
            #![allow(non_upper_case_globals)]
            #![allow(unused_imports)]
            #![allow(non_camel_case_types)]
            #![allow(dead_code)]
            #![allow(unused_variables)]
            #![allow(clippy::match_single_binding)]
            #![allow(clippy::too_many_arguments)]
            #![allow(clippy::type_complexity)]
            #![allow(clippy::single_match)]

            use std::os::unix::io::RawFd;
            use crate::wire::{WireMessage, MessageBuilder, Action, ProtocolError};
            use crate::state::Context;

            pub const ALLOWED_INTERFACES: &[&str] = &[
                #(#interface_names),*
            ];

            #(#parts)*

            pub trait ProtocolHandler:
                #(#handler_trait_names +)*
            {}

            pub fn dispatch_request<H: ProtocolHandler + ?Sized>(
                interface: &str,
                msg: &mut WireMessage,
                handler: &mut H,
                ctx: &mut Context,
            ) -> Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError> {
                ctx.last_sender_id = msg.sender_id;
                match interface {
                    #(#dispatch_request_cases)*
                    _ => Ok(None),
                }
            }

            pub fn dispatch_event<H: ProtocolHandler + ?Sized>(
                interface: &str,
                msg: &mut WireMessage,
                handler: &mut H,
                ctx: &mut Context,
            ) -> Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError> {
                ctx.last_sender_id = msg.sender_id;
                match interface {
                    #(#dispatch_event_cases)*
                    _ => Ok(None),
                }
            }

            #delegation_macro
        }
    };

    format_rust_code(&expanded.to_string())
}

fn format_rust_code(code: &str) -> String {
    if let Ok(mut child) = Command::new("rustfmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(code.as_bytes());
        }
        if let Ok(output) = child.wait_with_output()
            && output.status.success()
            && let Ok(formatted) = String::from_utf8(output.stdout)
        {
            return formatted;
        }
    }
    code.to_string()
}

fn map_type(arg: &Arg) -> TokenStream {
    match arg.typ.as_str() {
        "int" => quote! { i32 },
        "uint" => quote! { u32 },
        "fixed" => quote! { f32 },
        "string" => quote! { String },
        "object" => quote! { u32 },
        "new_id" => {
            if arg.interface.is_none() {
                quote! { (String, u32, u32) }
            } else {
                quote! { u32 }
            }
        }
        "array" => quote! { Vec<u8> },
        "fd" => quote! { RawFd },
        _ => quote! { u32 },
    }
}

fn map_type_fq(arg: &Arg) -> TokenStream {
    match arg.typ.as_str() {
        "fd" => quote! { std::os::unix::io::RawFd },
        // For other types, map_type returns primitives or standard library types (String, Vec)
        // or tuples of them, so it's safe to reuse.
        _ => map_type(arg),
    }
}

fn map_read_fn(arg: &Arg) -> TokenStream {
    match arg.typ.as_str() {
        "int" => quote! { msg.read_i32()? },
        "uint" => quote! { msg.read_u32()? },
        "fixed" => quote! { msg.read_fixed()? },
        "string" => quote! { msg.read_string()? },
        "object" => quote! { msg.read_u32()? },
        "new_id" => {
            if arg.interface.is_none() {
                quote! { (msg.read_string()?, msg.read_u32()?, msg.read_u32()?) }
            } else {
                quote! { msg.read_u32()? }
            }
        }
        "array" => quote! { msg.read_array()? },
        "fd" => quote! { msg.read_fd()? },
        _ => quote! { msg.read_u32()? },
    }
}

fn map_write_fn(arg: &Arg, name: &Ident) -> TokenStream {
    match arg.typ.as_str() {
        "int" => quote! { builder.write_i32(#name) },
        "uint" | "object" => quote! { builder.write_u32(#name) },
        "new_id" => {
            if arg.interface.is_none() {
                quote! {
                    builder.write_string(&#name.0);
                    builder.write_u32(#name.1);
                    builder.write_u32(#name.2);
                }
            } else {
                quote! { builder.write_u32(#name) }
            }
        }
        "fixed" => quote! { builder.write_fixed(#name) },
        "string" => quote! { builder.write_string(&#name) },
        "array" => quote! { builder.write_array(&#name) },
        "fd" => quote! { builder.write_fd(#name) },
        _ => quote! { builder.write_u32(#name) },
    }
}

fn generate_interface(interface: &Interface) -> TokenStream {
    let mod_name = format_ident!("{}", interface.name);
    let handler_trait_name = format_ident!("{}Handler", snake_to_camel(&interface.name));

    let mut request_opcodes = Vec::new();
    let mut event_opcodes = Vec::new();

    let mut request_variants = Vec::new();
    let mut event_variants = Vec::new();

    let mut request_decoders = Vec::new();
    let mut event_decoders = Vec::new();

    let mut request_opcode_match = Vec::new();
    let mut event_opcode_match = Vec::new();

    let mut handler_methods = Vec::new();
    let mut dispatch_request_arms = Vec::new();
    let mut dispatch_event_arms = Vec::new();

    let mut req_idx = 0u16;
    let mut evt_idx = 0u16;

    for item in &interface.items {
        match item {
            InterfaceItem::Request(req) => {
                let opcode_name = format_ident!("REQ_{}", req.name.to_uppercase());
                request_opcodes.push(quote! { pub const #opcode_name: u16 = #req_idx; });

                let var_name = format_ident!("{}", snake_to_camel(&req.name));
                let fields = generate_fields(&req.items);
                request_variants.push(quote! { #var_name { #fields } });

                let decodes = generate_reads(&req.items);
                let field_names = generate_field_names(&req.items);
                request_decoders.push(quote! {
                    #req_idx => {
                        #decodes
                        Ok(Request::#var_name { #field_names })
                    }
                });

                request_opcode_match.push(quote! {
                    Request::#var_name { .. } => #req_idx,
                });

                // Handler method
                let method_name = format_ident!("on_{}", req.name);
                let method_args = generate_handler_args(&req.items);
                handler_methods.push(quote! {
                    fn #method_name(&mut self, _ctx: &mut Context, #method_args) -> Action {
                        Action::Forward
                    }
                });

                // Dispatch arm
                let mut mapping_and_writing = Vec::new();
                let mut handler_call_args = Vec::new();
                let mut field_names_list = Vec::new();

                for arg_item in &req.items {
                    if let MessageItem::Arg(arg) = arg_item {
                        let name = sanitize_ident(&arg.name);
                        field_names_list.push(name.clone());

                        if arg.typ == "string"
                            || arg.typ == "array"
                            || (arg.typ == "new_id" && arg.interface.is_none())
                        {
                            handler_call_args.push(quote! { &#name });
                        } else {
                            handler_call_args.push(quote! { #name });
                        }

                        match arg.typ.as_str() {
                            "object" => {
                                let is_nullable = arg.allow_null.unwrap_or(false);
                                if is_nullable {
                                    mapping_and_writing.push(quote! {
                                        let host_id = ctx.shadow_table.get_host_id(#name).unwrap_or(0);
                                        builder.write_u32(host_id);
                                    });
                                } else {
                                    let arg_name_str = &arg.name;
                                    let req_name_str = &req.name;
                                    mapping_and_writing.push(quote! {
                                        let host_id = if let Some(id) = ctx.shadow_table.get_host_id(#name) {
                                            id
                                        } else {
                                            log::debug!("Dropping request {} due to missing mapping for non-nullable argument {}", #req_name_str, #arg_name_str);
                                            return Ok(None);
                                        };
                                        builder.write_u32(host_id);
                                    });
                                }
                            }
                            "new_id" => {
                                if let Some(ref interface_name) = arg.interface {
                                    mapping_and_writing.push(quote! {
                                        let host_id = ctx.shadow_table.allocate_host_id();
                                        ctx.shadow_table.map_id(#name, host_id);
                                        ctx.shadow_table.track_interface(#name, #interface_name.to_string());
                                        builder.write_u32(host_id);
                                    });
                                } else {
                                    mapping_and_writing.push(quote! {
                                        let (ref interface_name, version, id) = #name;
                                        builder.write_string(interface_name);
                                        builder.write_u32(version);
                                        let host_id = ctx.shadow_table.allocate_host_id();
                                        ctx.shadow_table.map_id(id, host_id);
                                        ctx.shadow_table.track_interface(id, interface_name.clone());
                                        builder.write_u32(host_id);
                                    });
                                }
                            }
                            _ => {
                                let write_call = map_write_fn(arg, &name);
                                mapping_and_writing.push(quote! { #write_call; });
                            }
                        }
                    }
                }

                let is_destructor = req.msg_type.as_deref() == Some("destructor");
                let destructor_cleanup = if is_destructor {
                    quote! { ctx.shadow_table.remove_id(msg.sender_id); }
                } else {
                    quote! {}
                };

                dispatch_request_arms.push(quote! {
                    Request::#var_name { #(#field_names_list),* } => {
                        if handler.#method_name(ctx, #(#handler_call_args),*) == Action::Forward {
                            #[allow(unused_mut)]
                            let mut builder = MessageBuilder::new();
                            #(#mapping_and_writing)*
                            let host_sender_id = ctx.shadow_table.get_host_id(msg.sender_id).unwrap_or(0);
                            let mut full_msg = Vec::new();
                            full_msg.extend_from_slice(&host_sender_id.to_ne_bytes());
                            let len = (builder.payload.len() + 8) as u32;
                            let word2 = (len << 16) | (#req_idx as u32);
                            full_msg.extend_from_slice(&word2.to_ne_bytes());
                            full_msg.extend_from_slice(&builder.payload);
                            #destructor_cleanup
                            return Ok(Some((full_msg, builder.fds)));
                        }
                    }
                });

                req_idx += 1;
            }
            InterfaceItem::Event(evt) => {
                let opcode_name = format_ident!("EVT_{}", evt.name.to_uppercase());
                event_opcodes.push(quote! { pub const #opcode_name: u16 = #evt_idx; });

                let var_name = format_ident!("{}", snake_to_camel(&evt.name));
                let fields = generate_fields(&evt.items);
                event_variants.push(quote! { #var_name { #fields } });

                let decodes = generate_reads(&evt.items);
                let field_names = generate_field_names(&evt.items);
                event_decoders.push(quote! {
                    #evt_idx => {
                        #decodes
                        Ok(Event::#var_name { #field_names })
                    },
                });

                event_opcode_match.push(quote! {
                    Event::#var_name { .. } => #evt_idx,
                });

                // Handler method for event
                let method_name = format_ident!("on_{}", evt.name);
                let method_args = generate_handler_args(&evt.items);
                handler_methods.push(quote! {
                    fn #method_name(&mut self, _ctx: &mut Context, #method_args) -> Action {
                        Action::Forward
                    }
                });

                // Dispatch arm for event (Host -> Guest)
                let mut mapping_and_writing = Vec::new();
                let mut handler_call_args = Vec::new();
                let mut field_names_list = Vec::new();

                for arg_item in &evt.items {
                    if let MessageItem::Arg(arg) = arg_item {
                        let name = sanitize_ident(&arg.name);
                        field_names_list.push(name.clone());

                        if arg.typ == "string"
                            || arg.typ == "array"
                            || (arg.typ == "new_id" && arg.interface.is_none())
                        {
                            handler_call_args.push(quote! { &#name });
                        } else {
                            handler_call_args.push(quote! { #name });
                        }

                        match arg.typ.as_str() {
                            "object" => {
                                let is_nullable = arg.allow_null.unwrap_or(false);
                                if is_nullable {
                                    mapping_and_writing.push(quote! {
                                        let guest_id = ctx.shadow_table.get_guest_id(#name).unwrap_or(0);
                                        builder.write_u32(guest_id);
                                    });
                                } else {
                                    let arg_name_str = &arg.name;
                                    let evt_name_str = &evt.name;
                                    mapping_and_writing.push(quote! {
                                        let guest_id = if let Some(id) = ctx.shadow_table.get_guest_id(#name) {
                                            id
                                        } else {
                                            log::debug!("Dropping event {} due to missing mapping for non-nullable argument {}", #evt_name_str, #arg_name_str);
                                            return Ok(None);
                                        };
                                        builder.write_u32(guest_id);
                                    });
                                }
                            }
                            "new_id" => {
                                if let Some(ref interface_name) = arg.interface {
                                    mapping_and_writing.push(quote! {
                                        ctx.shadow_table.map_id(#name, #name);
                                        ctx.shadow_table.track_interface(#name, #interface_name.to_string());
                                        builder.write_u32(#name);
                                    });
                                } else {
                                    mapping_and_writing.push(quote! {
                                        builder.write_u32(#name);
                                    });
                                }
                            }
                            _ => {
                                let write_call = map_write_fn(arg, &name);
                                mapping_and_writing.push(quote! { #write_call; });
                            }
                        }
                    }
                }

                let is_destructor = evt.msg_type.as_deref() == Some("destructor");
                let destructor_cleanup = if is_destructor {
                    quote! {
                        let guest_sender_id = ctx.shadow_table.get_guest_id(msg.sender_id).unwrap_or(0);
                        ctx.shadow_table.remove_id(guest_sender_id);
                    }
                } else {
                    quote! {}
                };

                dispatch_event_arms.push(quote! {
                    Event::#var_name { #(#field_names_list),* } => {
                        if handler.#method_name(ctx, #(#handler_call_args),*) == Action::Forward {
                            #[allow(unused_mut)]
                            let mut builder = MessageBuilder::new();
                            #(#mapping_and_writing)*
                            let guest_sender_id = ctx.shadow_table.get_guest_id(msg.sender_id).unwrap_or(0);
                            let mut full_msg = Vec::new();
                            full_msg.extend_from_slice(&guest_sender_id.to_ne_bytes());
                            let len = (builder.payload.len() + 8) as u32;
                            let word2 = (len << 16) | (#evt_idx as u32);
                            full_msg.extend_from_slice(&word2.to_ne_bytes());
                            full_msg.extend_from_slice(&builder.payload);
                            #destructor_cleanup
                            return Ok(Some((full_msg, builder.fds)));
                        }
                    }
                });

                evt_idx += 1;
            }
            _ => {}
        }
    }

    quote! {
        pub mod #mod_name {
            use super::*;

            #(#request_opcodes)*
            #(#event_opcodes)*

            #[derive(Debug)]
            pub enum Request {
                #(#request_variants),*
            }

            impl Request {
                pub fn from_wire(msg: &mut WireMessage) -> Result<Self, ProtocolError> {
                    match msg.opcode {
                        #(#request_decoders)*
                        _ => Err(ProtocolError::UnknownOpcode(msg.opcode))
                    }
                }

                pub fn opcode(&self) -> u16 {
                    match self {
                        #(#request_opcode_match)*
                        #[allow(unreachable_patterns)]
                        _ => unreachable!()
                    }
                }
            }

            #[derive(Debug)]
            pub enum Event {
                #(#event_variants),*
            }

             impl Event {
                pub fn from_wire(msg: &mut WireMessage) -> Result<Self, ProtocolError> {
                    match msg.opcode {
                        #(#event_decoders)*
                        _ => Err(ProtocolError::UnknownOpcode(msg.opcode))
                    }
                }

                pub fn opcode(&self) -> u16 {
                    match self {
                        #(#event_opcode_match)*
                        #[allow(unreachable_patterns)]
                        _ => unreachable!()
                    }
                }
            }

            pub trait #handler_trait_name {
                #(#handler_methods)*
            }

            pub fn dispatch_request<H: #handler_trait_name + ?Sized>(
                msg: &mut WireMessage,
                handler: &mut H,
                ctx: &mut Context,
            ) -> Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError> {
                let req = Request::from_wire(msg)?;
                match req {
                    #(#dispatch_request_arms)*
                    #[allow(unreachable_patterns)]
                    _ => {}
                }
                Ok(None)
            }

            pub fn dispatch_event<H: #handler_trait_name + ?Sized>(
                msg: &mut WireMessage,
                handler: &mut H,
                ctx: &mut Context,
            ) -> Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError> {
                let evt = Event::from_wire(msg)?;
                match evt {
                    #(#dispatch_event_arms)*
                    #[allow(unreachable_patterns)]
                    _ => {}
                }
                Ok(None)
            }
        }
    }
}

fn generate_delegation_macro(protocol: &Protocol) -> TokenStream {
    let mut dispatch_arms = Vec::new();
    let mut entry_point_calls = Vec::new();
    let protocol_name = format_ident!("{}", protocol.name);

    for item in &protocol.items {
        if let ProtocolItem::Interface(interface) = item {
            let iface_name = &interface.name;
            let iface_ident = format_ident!("{}", iface_name);
            let mod_name = format_ident!("{}", iface_name);
            let handler_trait_name = format_ident!("{}Handler", snake_to_camel(iface_name));

            // Entry point call
            entry_point_calls.push(quote! {
                $crate::protocols::#protocol_name::impl_sommelier_delegates!(@dispatch $target, #iface_ident, [ $($iface : $field,)* ]);
            });

            let mut method_impls = Vec::new();
            for item in &interface.items {
                let (name, args_items) = match item {
                    InterfaceItem::Request(req) => (&req.name, &req.items),
                    InterfaceItem::Event(evt) => (&evt.name, &evt.items),
                    _ => continue,
                };

                let method_name = format_ident!("on_{}", name);
                let sig_args = generate_handler_args_fq(args_items);
                let call_args = generate_forwarding_call_args(args_items);

                method_impls.push(quote! {
                     fn #method_name(&mut self, ctx: &mut $crate::state::Context, #sig_args) -> $crate::wire::Action {
                         self.$field.#method_name(ctx, #call_args)
                     }
                 });
            }

            dispatch_arms.push(quote! {
                (@dispatch $target:ty, #iface_ident, [ #iface_ident : $field:ident, $($rest:tt)* ]) => {
                    impl $crate::protocols::#protocol_name::#mod_name::#handler_trait_name for $target {
                        #(#method_impls)*
                    }
                };
                (@dispatch $target:ty, #iface_ident, [ $other:ident : $field:ident, $($rest:tt)* ]) => {
                    $crate::protocols::#protocol_name::impl_sommelier_delegates!(@dispatch $target, #iface_ident, [ $($rest)* ]);
                };
                (@dispatch $target:ty, #iface_ident, []) => {
                    impl $crate::protocols::#protocol_name::#mod_name::#handler_trait_name for $target {}
                };
            });
        }
    }

    quote! {
        macro_rules! impl_sommelier_delegates {
            ($target:ty, { $($iface:ident : $field:ident),* }) => {
                #( #entry_point_calls )*
            };
            #(#dispatch_arms)*
        }
        pub(crate) use impl_sommelier_delegates;
    }
}

fn generate_reads(items: &[MessageItem]) -> TokenStream {
    let mut statements = Vec::new();
    for item in items {
        if let MessageItem::Arg(arg) = item {
            let name = sanitize_ident(&arg.name);
            let read_call = map_read_fn(arg);
            statements.push(quote! { let #name = #read_call; });
        }
    }
    quote! { #(#statements)* }
}

fn generate_field_names(items: &[MessageItem]) -> TokenStream {
    let mut names = Vec::new();
    for item in items {
        if let MessageItem::Arg(arg) = item {
            names.push(sanitize_ident(&arg.name));
        }
    }
    quote! { #(#names),* }
}

fn generate_fields(items: &[MessageItem]) -> TokenStream {
    let mut fields = Vec::new();
    for item in items {
        if let MessageItem::Arg(arg) = item {
            let name = sanitize_ident(&arg.name);
            let ty = map_type(arg);
            fields.push(quote! { #name: #ty });
        }
    }
    quote! { #(#fields),* }
}

fn generate_handler_args(items: &[MessageItem]) -> TokenStream {
    let mut args = Vec::new();
    for item in items {
        if let MessageItem::Arg(arg) = item {
            let name = format_ident!("_{}", sanitize_ident(&arg.name));
            let ty = map_type(arg);
            if arg.typ == "string" {
                args.push(quote! { #name: &#ty });
            } else if arg.typ == "array" {
                args.push(quote! { #name: &[u8] });
            } else if arg.typ == "new_id" && arg.interface.is_none() {
                args.push(quote! { #name: &(String, u32, u32) });
            } else {
                args.push(quote! { #name: #ty });
            }
        }
    }
    quote! { #(#args),* }
}

fn generate_handler_args_fq(items: &[MessageItem]) -> TokenStream {
    let mut args = Vec::new();
    for item in items {
        if let MessageItem::Arg(arg) = item {
            let name = format_ident!("_{}", sanitize_ident(&arg.name));
            let ty = map_type_fq(arg);
            if arg.typ == "string" {
                args.push(quote! { #name: &#ty });
            } else if arg.typ == "array" {
                args.push(quote! { #name: &[u8] });
            } else if arg.typ == "new_id" && arg.interface.is_none() {
                args.push(quote! { #name: &(String, u32, u32) });
            } else {
                args.push(quote! { #name: #ty });
            }
        }
    }
    quote! { #(#args),* }
}

fn generate_forwarding_call_args(items: &[MessageItem]) -> TokenStream {
    let mut args = Vec::new();
    for item in items {
        if let MessageItem::Arg(arg) = item {
            let name = format_ident!("_{}", sanitize_ident(&arg.name));
            args.push(quote! { #name });
        }
    }
    quote! { #(#args),* }
}

fn sanitize_ident(name: &str) -> Ident {
    match name {
        "type" | "move" | "loop" | "box" | "crate" | "match" | "fn" | "impl" | "trait" | "pub"
        | "mod" | "use" | "self" | "super" | "in" | "where" | "for" | "while" | "if" | "else"
        | "let" | "struct" | "enum" | "const" | "static" | "mut" | "ref" | "extern" | "unsafe"
        | "return" | "break" | "continue" | "async" | "await" | "dyn" | "abstract" | "become"
        | "do" | "final" | "macro" | "override" | "priv" | "typeof" | "unsized" | "virtual"
        | "yield" | "try" => format_ident!("r#{}", name),
        _ => format_ident!("{}", name),
    }
}

fn snake_to_camel(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}
