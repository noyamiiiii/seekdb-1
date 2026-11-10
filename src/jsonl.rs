//! Official JSON Lines transport. SQL parsing belongs to the engine, not the wire framer.
use std::io::{self, BufRead, Write};
use serde_json::Value;
use crate::{response::{Cell, Response}, runner::Handler};

fn json(out: &mut impl Write, value: &impl serde::Serialize) -> io::Result<()> {
    serde_json::to_writer(out, value).map_err(io::Error::other)
}

fn error(out: &mut impl Write, id: &Value, code: &str, message: &str) -> io::Result<()> {
    json(out, &serde_json::json!({"id":id,"ok":false,"error":{"code":code,"message":message}}))?;
    out.write_all(b"\n")
}

fn write_response(out: &mut impl Write, id: &Value, response: &Response) -> io::Result<()> {
    if let Response::Err(err) = response {
        let code = match err.code {
            crate::error::ErrorCode::DuplicateEntry => "duplicate_key".to_string(),
            crate::error::ErrorCode::NotSupported => "unsupported".to_string(),
            _ => err.code.as_u16().to_string(),
        };
        return error(out, id, &code, &err.message);
    }
    if let Response::Rows(rs) = response {
        let finite = rs.rows.iter().flatten().all(|cell| match cell {
            Cell::Real(x) => x.is_finite(),
            Cell::Vector(v) => v.iter().all(|x| x.is_finite()),
            _ => true,
        });
        if !finite { return error(out,id,"non_finite","non-finite result"); }
    }
    out.write_all(b"{\"id\":")?;
    json(out,id)?;
    out.write_all(b",\"ok\":true")?;
    match response {
        Response::Ok { affected } => { write!(out,",\"affected_rows\":{affected}")?; }
        Response::Rows(rs) => {
            out.write_all(b",\"columns\":")?; json(out,&rs.columns)?;
            out.write_all(b",\"rows\":[")?;
            for (i,row) in rs.rows.iter().enumerate() {
                if i>0 {out.write_all(b",")?;}
                out.write_all(b"[")?;
                for (j,cell) in row.iter().enumerate() {
                    if j>0 {out.write_all(b",")?;}
                    match cell {
                        Cell::Int(x) => json(out,x)?,
                        Cell::Real(x) => json(out,x)?,
                        Cell::Text(x) => json(out,x)?,
                        Cell::Vector(x) => { out.write_all(b"[")?; for (k,v) in x.iter().enumerate() { if k>0 { out.write_all(b",")?; } json(out,&f64::from(*v))?; } out.write_all(b"]")?; },
                    }
                }
                out.write_all(b"]")?;
            }
            out.write_all(b"]")?;
        }
        Response::Bye => {},
        Response::Err(_) => unreachable!(),
    }
    out.write_all(b"}\n")
}

pub fn serve<H: Handler, R: BufRead, W: Write>(handler: &mut H, mut input: R, out: &mut W) -> io::Result<()> {
    out.write_all(b"{\"type\":\"ready\"}\n")?;
    out.flush()?;
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 { return handler.shutdown_result(); }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => { error(out,&Value::Null,"invalid_request",&e.to_string())?; out.flush()?; continue; }
        };
        let id = request.get("id").unwrap_or(&Value::Null);
        if !request.is_object() || request.get("id").is_none() {
            error(out,id,"invalid_request","request must be an object with id")?;
        } else {
            match request.get("op").and_then(Value::as_str) {
                Some("shutdown") => {
                    // Acknowledge only after rollback and persistent checkpoint have completed.
                    handler.shutdown_result()?;
                    json(out,&serde_json::json!({"id":id,"ok":true}))?;
                    out.write_all(b"\n")?; out.flush()?;
                    return Ok(());
                }
                Some("execute") => {
                    let mode = request.get("search_mode").and_then(Value::as_str).unwrap_or("exact");
                    if let Some(sql) = request.get("sql").and_then(Value::as_str) {
                        if mode == "exact" || mode == "ann" {
                            // Both modes use exact search: there is no recall loss or stale ANN state.
                            let response = handler.execute(sql);
                            write_response(out,id,&response)?;
                        } else { error(out,id,"invalid_request","search_mode must be exact or ann")?; }
                    } else { error(out,id,"invalid_request","sql must be a string")?; }
                }
                _ => { error(out,id,"invalid_request","unknown op")?; }
            }
        }
        out.flush()?;
    }
}