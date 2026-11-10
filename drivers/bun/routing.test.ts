import { test } from 'bun:test'
import { strict as assert } from 'node:assert'
import { createServer, type Socket } from 'node:net'
import { connect } from './index'
import { frame } from './test/frame-fixture'

const string = (value: string) => { const text=Buffer.from(value); const size=Buffer.alloc(4);size.writeUInt32LE(text.length);return Buffer.concat([size,text]) }
function topology(primary: string, replica: string) {
  const epoch=Buffer.alloc(8),count=Buffer.alloc(4),lag=Buffer.alloc(4)
  count.writeUInt32LE(1);lag.writeUInt32LE(10)
  const body=Buffer.concat([epoch,string(primary),string(''),count,string(replica),string(''),Buffer.from([1]),lag,epoch,Buffer.from([0])])
  const header=Buffer.alloc(5);header[0]=1;header.writeUInt32LE(body.length,1)
  return Buffer.concat([header,body])
}

test('native Bun TCP discovers, retains bearer auth, pins and never replays effects',async()=>{
  const sockets=new Set<Socket>(), calls: [string,string][]=[]
  let primary='',replica=''
  const peer=async(label:string)=>{
    const server=createServer(socket=>{
      sockets.add(socket);socket.on('close',()=>sockets.delete(socket));socket.on('error',()=>{})
      let input=Buffer.alloc(0),preamble=false
      socket.on('data',chunk=>{
        input=Buffer.concat([input,chunk])
        if(!preamble){if(input.length<2)return;input=input.subarray(2);preamble=true}
        while(input.length>=16){
          const length=input.readUInt32LE(0);if(input.length<length)return
          const request=input.subarray(0,length);input=input.subarray(length)
          const kind=request[4],payload=request.subarray(16)
          let response:Buffer
          if(kind===0x16){socket.end();return}
          if(kind===0x10) response=frame('{"auth":"bearer","features":3}',0x11)
          else if(kind===0x13){assert.equal(JSON.parse(payload.toString()).token,'secret');response=frame('{"features":3}',0x14)}
          else if(kind===0x32){assert.equal(payload.toString(),'red');response=frame(topology(primary,replica))}
          else {
            assert.equal(kind,0x28)
            const sql=payload.subarray(4,4+payload.readUInt32LE(0)).toString();calls.push([label,sql])
            if(sql.includes('effectful') || sql==='UPDATE lost'){socket.end();return}
            response=frame(JSON.stringify({label}))
          }
          request.copy(response,8,8,16);socket.write(response)
        }
      })
    })
    await new Promise<void>(resolve=>server.listen(0,'127.0.0.1',resolve))
    const address=server.address();assert(address && typeof address==='object')
    return {server,address:`red://127.0.0.1:${address.port}`}
  }
  const writer=await peer('primary'),reader=await peer('replica')
  primary=writer.address;replica=reader.address
  let client:Awaited<ReturnType<typeof connect>>|undefined
  try{
    client=await connect(primary.slice(6),{token:'secret'})
    assert.equal(client.supportsParams(),true)
    assert.equal(JSON.parse(await client.query('SELECT 1')).label,'primary')
    assert.equal(JSON.parse(await client.queryWithOptions('SELECT 1',{consistency:'local'})).label,'replica')
    await client.query('BEGIN')
    assert.equal(JSON.parse(await client.queryWithOptions('SELECT 1',{consistency:'local'})).label,'primary')
    await client.query('ROLLBACK')
    for(const sql of ['UPDATE lost','SELECT "effectful"()']){
      const before=calls.length;await assert.rejects(client.queryWithOptions(sql,{consistency:'local'}))
      assert.deepEqual(calls.slice(before),[['primary',sql]])
    }
  }finally{
    client?.close();for(const socket of sockets)socket.destroy()
    await Promise.all([writer.server,reader.server].map(server=>new Promise<void>(resolve=>server.close(()=>resolve()))))
  }
})
