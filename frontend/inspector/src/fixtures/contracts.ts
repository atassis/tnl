import fixtures from '../api/generated/fixtures.json';
import {validate_detail,validate_list,validate_stats,validate_preview} from '../api/generated/validators';
export function detailFixture() {const value:unknown=structuredClone(fixtures.detail);if(!validate_detail(value))throw Error('Invalid synthetic detail');return value;}
export function listFixture() {const value:unknown=structuredClone(fixtures.list);if(!validate_list(value))throw Error('Invalid synthetic list');return value;}
export function statsFixture() {const value:unknown=structuredClone(fixtures.stats);if(!validate_stats(value))throw Error('Invalid synthetic stats');return value;}
export function previewFixture() {const value:unknown=structuredClone(fixtures.preview);if(!validate_preview(value))throw Error('Invalid synthetic preview');return value;}
